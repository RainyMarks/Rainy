/** Per-connection editor language servers over the `/rainy/ide/lsp` WebSocket. */
import { realpath } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import type { RawData, WebSocket } from 'ws'
import { z } from 'zod'
import { assertNever, brandString } from '../../shared/brand.ts'
import type { IdeExecutionConfiguration } from '../../shared/ide-execution-protocol.ts'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { ProcessHandle } from '../process.ts'
import type { ResolvedWorkspaceEnvironment } from '../runtime/environments.ts'
import type { IdeSubprocess } from './execution-process.ts'
import { nodeIdeExecutionFiles, resolveIdeWorkspacePath } from './execution-resolve.ts'
import { ideContains } from './files-core.ts'
import { ideLanguageSchema, type IdeLanguage } from './format.ts'
import { encodeMessage, MessageDecoder } from './jsonrpc-framing.ts'
import type { IdeToolPaths } from './tools.ts'

/** Language-process and transport budgets. */
export interface IdeLanguageLimits {
  readonly maxConnections: number
  readonly maxMessageBytes: number
  readonly maxQueuedBytes: number
  readonly killGraceMs: number
  readonly shutdownMs: number
}

/** A complete process choice resolved before the server starts. */
export interface IdeLanguageSpec {
  readonly workspaceId: WorkspaceId
  readonly root: string
  readonly language: IdeLanguage
  readonly argv: readonly string[]
  readonly environment?: Readonly<Record<string, string>> | undefined
  /** `tsserver.js` offered as `initializationOptions.tsserver.fallbackPath` when the editor names none. */
  readonly tsserver?: string | undefined
}

/** Validated messages remain JSON objects; servers own their protocol-specific fields. */
export type IdeLanguageMessage = Record<string, unknown>

const languageQuery = z.object({
  workspaceId: z.string().min(1).max(512).transform(value => brandString<WorkspaceId>(value)),
  language: ideLanguageSchema,
  rootId: z.string().min(1).max(128).transform(value => brandString<IdeRootId>(value)).optional(),
}).strict()

const OPEN = 1

function object(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

async function checkFileUri(root: string, spelling: string): Promise<void> {
  const uri = new URL(spelling)
  if (uri.protocol !== 'file:') throw new Error('Editor language documents must use file URIs')
  const path = fileURLToPath(uri)
  if (!ideContains(root, resolve(path))) throw new Error('Language document is outside its workspace')
  let existing = path
  for (;;) {
    try {
      if (!ideContains(root, await realpath(existing))) throw new Error('Language document points outside its workspace')
      return
    } catch (error) {
      if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error
      const parent = dirname(existing)
      if (parent === existing) throw error
      existing = parent
    }
  }
}

/**
 * Validate an editor message and bind initialization to its project directory.
 * @param message Untrusted decoded WebSocket message.
 * @param spec Canonical project directory and chosen language process.
 * @returns An LSP message with the Host's workspace identity.
 */
export async function prepareIdeLanguageMessage(message: unknown, spec: IdeLanguageSpec): Promise<IdeLanguageMessage> {
  if (!object(message) || message.jsonrpc !== '2.0'
    || !('id' in message || typeof message.method === 'string')
    || ('id' in message && typeof message.id !== 'number' && typeof message.id !== 'string' && message.id !== null)) throw new Error('Invalid editor language message')
  if (message.method === 'initialize') {
    if (!object(message.params)) throw new Error('Language initialization parameters are missing')
    const uri = pathToFileURL(spec.root).href
    const params: Record<string, unknown> = { ...message.params, processId: null, rootPath: spec.root, rootUri: uri,
      workspaceFolders: [{ uri, name: spec.root.split(/[\\/]/).at(-1) || spec.root }] }
    if (spec.tsserver !== undefined) {
      const options = object(params.initializationOptions) ? params.initializationOptions : {}
      const tsserver = object(options.tsserver) ? options.tsserver : {}
      params.initializationOptions = { ...options, tsserver: { fallbackPath: spec.tsserver, ...tsserver } }
    }
    return { ...message, params }
  }
  if (message.method === 'workspace/didChangeWorkspaceFolders') throw new Error('Open another workspace language connection to change the project')
  const pending: unknown[] = [message.params]
  while (pending.length) {
    const value = pending.pop()
    if (Array.isArray(value)) {
      const entries: readonly unknown[] = value
      pending.push(...entries)
      continue
    }
    if (!object(value)) continue
    for (const [key, child] of Object.entries(value)) {
      if (['uri', 'oldUri', 'newUri', 'rootUri'].includes(key) && typeof child === 'string') await checkFileUri(spec.root, child)
      else if (typeof child === 'object' && child !== null) pending.push(child)
    }
  }
  return message
}

interface Connection {
  readonly socket: WebSocket
  readonly child: ProcessHandle
  readonly workspaceId: WorkspaceId
  closePromise?: Promise<void>
  closing: boolean
}

interface Received { readonly data: RawData; readonly binary: boolean }

function bytesOf(data: RawData): Buffer {
  return Array.isArray(data) ? Buffer.concat(data) : data instanceof ArrayBuffer ? Buffer.from(data) : data
}

/** One language-server process per editor connection; unsaved documents stay open until the editor closes them. */
export class IdeLanguageService {
  private readonly connections = new Set<Connection>()
  private closing = false

  /** @param options Process provider, project resolution, tool paths and bounds. */
  constructor(private readonly options: {
    readonly subprocess: Pick<IdeSubprocess, 'spawn' | 'resolveExecutable'>
    readonly resolveWorkspace: (id: WorkspaceId, rootId?: IdeRootId) => Promise<{ readonly root: string }>
    readonly resolveEnvironment?: ((id: WorkspaceId) => ResolvedWorkspaceEnvironment) | undefined
    readonly assertUsable: () => void
    readonly configuration: (id: WorkspaceId) => Promise<IdeExecutionConfiguration | undefined>
    readonly tools: IdeToolPaths
    readonly limits: IdeLanguageLimits
    readonly log: (message: string) => void
  }) {}

  /**
   * Resolve a language server without starting it.
   * @param workspaceId Project identity.
   * @param language One of the editor languages.
   * @param rootId Mounted directory; omitted means the primary directory.
   * @returns The process launch.
   */
  async resolve(workspaceId: WorkspaceId, language: IdeLanguage, rootId?: IdeRootId): Promise<IdeLanguageSpec> {
    this.options.assertUsable()
    if (this.closing) throw new Error('Editor language services are closing')
    const { root } = await this.options.resolveWorkspace(workspaceId, rootId)
    const environment = this.options.resolveEnvironment?.(workspaceId).environment
    const tools = this.options.tools
    switch (language) {
      case 'python': return { workspaceId, root, language, argv: [tools.node, tools.pyright, '--stdio'], environment }
      case 'javascript': case 'typescript':
        return { workspaceId, root, language, argv: [tools.node, tools.tsServer, '--stdio'], environment, tsserver: tools.tsserver }
      case 'c': case 'cpp': {
        const configuration = await this.options.configuration(workspaceId)
        const active = configuration?.profiles.find(profile => profile.name === configuration.activeProfile)
        const profile = active?.language === 'c' || active?.language === 'cpp' ? active
          : configuration?.profiles.find(candidate => candidate.language === 'c' || candidate.language === 'cpp')
        const buildDirectory = profile?.build?.kind === 'cmake'
          ? await resolveIdeWorkspacePath(nodeIdeExecutionFiles, root, profile.build.buildDirectory) : undefined
        const argv = [await this.options.subprocess.resolveExecutable('clangd', environment), '--background-index=0',
          ...(buildDirectory === undefined ? [] : [`--compile-commands-dir=${buildDirectory}`])]
        return { workspaceId, root, language, argv, environment }
      }
      default: return assertNever(language)
    }
  }

  /**
   * Serve one accepted WebSocket whose query names the project, root and language.
   * Messages received while the server is being resolved are kept and forwarded in order.
   * @param socket Authenticated same-origin WebSocket.
   * @param url Upgrade request URL.
   */
  accept(socket: WebSocket, url: URL): void {
    const early: Received[] = []
    let earlyBytes = 0
    const hold = (data: RawData, binary: boolean): void => {
      earlyBytes += bytesOf(data).length
      if (earlyBytes > this.options.limits.maxQueuedBytes) socket.close(1009, 'Editor language input exceeds its message budget')
      else early.push({ data, binary })
    }
    socket.on('message', hold)
    void (async () => {
      let spec: IdeLanguageSpec
      try {
        const query = languageQuery.parse(Object.fromEntries(url.searchParams))
        spec = await this.resolve(query.workspaceId, query.language, query.rootId)
      } catch (error) {
        socket.close(1008, 'Invalid editor language request')
        this.options.log(`IDE language request rejected: ${error instanceof Error ? error.message : String(error)}`)
        return
      }
      socket.off('message', hold)
      if (socket.readyState !== OPEN) return
      try { this.attach(socket, spec, early) } catch (error) {
        socket.close(1011, 'Language server could not start')
        this.options.log(`IDE language startup failed: ${error instanceof Error ? error.stack ?? error.message : String(error)}`)
      }
    })()
  }

  /**
   * Bind one socket to a new language-server process.
   * @param socket Accepted WebSocket.
   * @param spec Resolved process.
   * @param early Messages received before the process existed.
   */
  attach(socket: WebSocket, spec: IdeLanguageSpec, early: readonly Received[] = []): void {
    this.options.assertUsable()
    if (this.closing || this.connections.size >= this.options.limits.maxConnections) {
      socket.close(1013, 'Editor language capacity reached')
      return
    }
    const limits = this.options.limits
    const child = this.options.subprocess.spawn({ argv: spec.argv, cwd: spec.root, stdin: 'pipe', graceMs: limits.killGraceMs,
      environment: { ...spec.environment, PYTHONDONTWRITEBYTECODE: '1' } })
    child.stderr.resume()
    const connection: Connection = { socket, child, workspaceId: spec.workspaceId, closing: false }
    this.connections.add(connection)
    const stop = (): void => {
      void this.closeConnection(connection).catch((error: unknown) => { this.options.log(`Editor language shutdown failed: ${String(error)}`) })
    }
    const fail = (error: unknown): void => {
      if (connection.closing) return
      if (socket.readyState === OPEN) socket.send(JSON.stringify({ jsonrpc: '2.0', method: 'window/logMessage', params: {
        type: 1, message: error instanceof Error ? error.message : 'Editor language connection failed',
      } }))
      socket.close(1011, 'Editor language connection failed')
      stop()
    }
    socket.once('close', stop)
    socket.once('error', fail)
    const decoder = new MessageDecoder(limits.maxMessageBytes)
    child.stdout.on('data', (chunk: Buffer) => {
      if (connection.closing) return
      try {
        for (const message of decoder.push(chunk)) {
          if (socket.bufferedAmount > limits.maxQueuedBytes) throw new Error('Editor language output is backpressured')
          if (socket.readyState === OPEN) socket.send(JSON.stringify(message))
        }
      } catch (error) { fail(error) }
    })
    let queuedBytes = 0
    let inbound: Promise<void> = Promise.resolve()
    const receive = (data: RawData, binary: boolean): void => {
      if (connection.closing) return
      const bytes = bytesOf(data)
      if (binary || bytes.length > limits.maxMessageBytes || queuedBytes + bytes.length > limits.maxQueuedBytes) {
        fail(new Error('Editor language input exceeds its message budget'))
        return
      }
      queuedBytes += bytes.length
      inbound = inbound.then(async () => {
        if (connection.closing) return
        const message: unknown = JSON.parse(bytes.toString('utf8'))
        const admitted = await prepareIdeLanguageMessage(message, spec)
        if (connection.closing) return
        const input = child.stdin
        if (!input || input.destroyed) throw new Error('Language server input is closed')
        const framed = encodeMessage(admitted)
        if (input.writableLength + framed.length > limits.maxQueuedBytes) throw new Error('Language server input is backpressured')
        await new Promise<void>((accept, reject) => { input.write(framed, (error) => { if (error) reject(error); else accept() }) })
      }).catch(fail).finally(() => { queuedBytes -= bytes.length })
    }
    socket.on('message', receive)
    for (const message of early) receive(message.data, message.binary)
    void child.done.then((outcome) => {
      if (!connection.closing) socket.close(outcome.exitCode === 0 ? 1000 : 1011, outcome.exitCode === 0 ? 'Language server exited' : 'Language server stopped')
      stop()
    }, fail)
  }

  /**
   * Stop the language servers of one project.
   * @param workspaceId Project identity.
   * @returns Completion after their processes stopped.
   */
  async closeWorkspace(workspaceId: WorkspaceId): Promise<void> {
    await Promise.allSettled([...this.connections].filter(connection => connection.workspaceId === workspaceId)
      .map(connection => this.closeConnection(connection)))
  }

  private closeConnection(connection: Connection): Promise<void> {
    if (connection.closePromise) return connection.closePromise
    connection.closing = true
    connection.closePromise = (async () => {
      connection.socket.removeAllListeners('message')
      connection.child.stdout.removeAllListeners('data')
      connection.socket.close()
      connection.child.terminate()
      const quiet = await connection.child.waitForExit(AbortSignal.timeout(this.options.limits.shutdownMs))
      this.connections.delete(connection)
      if (!quiet) throw new Error('Editor language process did not stop within its shutdown budget')
    })()
    return connection.closePromise
  }

  /** @returns Completion after every language-server process has stopped. */
  async close(): Promise<void> {
    this.closing = true
    const results = await Promise.allSettled([...this.connections].map(connection => this.closeConnection(connection)))
    const failures = results.filter((result): result is PromiseRejectedResult => result.status === 'rejected')
    if (failures.length) throw new AggregateError(failures.map((result): unknown => result.reason), 'Editor language shutdown failed')
  }
}
