/** Workspace-owned, persistent editor LSP connections over authenticated WebSockets. */
import { dirname, resolve } from 'node:path'
import { realpath } from 'node:fs/promises'
import { fileURLToPath, pathToFileURL } from 'node:url'
import type { SubprocessHandle, SubprocessRuntime } from '@deepseek-ai/dsh-subprocess'
import type { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type { IdeRootId } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'
import type { ResolvedWorkspaceEnvironment } from './runtime-environments.ts'
import { encodeMessage, MessageDecoder } from '@deepseek-ai/dsh-lsp-stdio'
import type WebSocket from 'ws'
import type { RawData } from 'ws'
import type { IdeLanguage } from './ide-format.ts'
import type { IdeToolPaths } from './ide-tools.ts'
import { ideContains } from './ide-files-core.ts'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { IdeExecutionConfiguration } from '@deepseek-ai/dsh-client-ui-rainy/ide-execution-protocol'
import { nodeIdeExecutionFiles, resolveIdeWorkspacePath } from './ide-execution-resolve.ts'

/** Language-process and transport budgets resolved at plugin construction. */
export interface IdeLanguageLimits {
  readonly maxConnections: number
  readonly maxMessageBytes: number
  readonly maxQueuedBytes: number
  readonly maxStderrBytes: number
  readonly killGraceMs: number
  readonly shutdownMs: number
}

/** A complete process choice resolved before accepting the WebSocket. */
export interface IdeLanguageSpec {
  readonly workspaceId: WorkspaceId
  readonly root: string
  readonly language: IdeLanguage
  readonly argv: readonly string[]
  readonly environment?: Readonly<Record<string, string>>
}

/** Validated messages remain JSON objects; servers own their protocol-specific fields. */
export type IdeLanguageMessage = Record<string, unknown>

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

/** Validate an editor message and bind initialization to its admitted workspace.
 * @param message - untrusted decoded WebSocket message.
 * @param spec - canonical project and chosen language process.
 * @returns an LSP message with the Host's workspace identity.
 */
export async function prepareIdeLanguageMessage(message: unknown, spec: IdeLanguageSpec): Promise<IdeLanguageMessage> {
  if (!object(message) || message.jsonrpc !== '2.0'
    || !('id' in message || typeof message.method === 'string')
    || ('id' in message && typeof message.id !== 'number' && typeof message.id !== 'string' && message.id !== null)) throw new Error('Invalid editor language message')
  if (message.method === 'initialize') {
    if (!object(message.params)) throw new Error('Language initialization parameters are missing')
    const uri = pathToFileURL(spec.root).href
    return { ...message, params: { ...message.params, processId: null, rootPath: spec.root, rootUri: uri,
      workspaceFolders: [{ uri, name: spec.root.split(/[\\/]/).at(-1) || spec.root }] } }
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
  readonly child: SubprocessHandle
  closePromise?: Promise<void>
  closing: boolean
}

/** One managed process per admitted editor connection; unsaved documents stay open until the client closes them. */
export class IdeLanguageService {
  private readonly connections = new Set<Connection>()
  private closing = false

  /** @param options - explicit execution provider, project resolver, resources and bounds. */
  constructor(private readonly options: {
    readonly subprocess: SubprocessRuntime
    readonly resolveWorkspace: (id: WorkspaceId, rootId?: IdeRootId) => Promise<{ readonly root: string }>
    readonly resolveEnvironment?: (id: WorkspaceId) => ResolvedWorkspaceEnvironment
    readonly assertUsable: () => void
    readonly configuration: (id: WorkspaceId) => IdeExecutionConfiguration | undefined
    readonly tools: IdeToolPaths
    readonly limits: IdeLanguageLimits
  }) {}

  /** Resolve a language server without starting it.
   * @param workspaceId - durable workspace identity.
   * @param language - one of the shipped IDE languages.
   * @returns the fully specified process launch.
   */
  async resolve(workspaceId: WorkspaceId, language: IdeLanguage, rootId?: IdeRootId): Promise<IdeLanguageSpec> {
    this.options.assertUsable()
    if (this.closing) throw new Error('Editor language services are closing')
    const { root } = await this.options.resolveWorkspace(workspaceId, rootId)
    const environment = this.options.resolveEnvironment?.(workspaceId).environment
    const tools = this.options.tools
    let argv: readonly string[]
    switch (language) {
      case 'python': argv = [tools.node, tools.pyright, '--stdio']; break
      case 'javascript': case 'typescript': argv = [tools.node, tools.tsServer, '--stdio']; break
      case 'c': case 'cpp': {
        const configuration = this.options.configuration(workspaceId)
        const active = configuration?.profiles.find(profile => profile.name === configuration.activeProfile)
        const profile = active?.language === 'c' || active?.language === 'cpp' ? active
          : configuration?.profiles.find(profile => profile.language === 'c' || profile.language === 'cpp')
        const buildDirectory = profile?.build?.kind === 'cmake'
          ? await resolveIdeWorkspacePath(nodeIdeExecutionFiles, root, profile.build.buildDirectory) : undefined
        argv = [await this.options.subprocess.resolveExecutable('clangd', environment), '--background-index=0',
          ...(buildDirectory === undefined ? [] : [`--compile-commands-dir=${buildDirectory}`])]
        break
      }
      default: return assertNever(language)
    }
    return { workspaceId, root, language, argv, environment }
  }

  /** Bind one accepted socket to a managed language process.
   * @param socket - authenticated WebSocket whose query selected the project.
   * @param spec - previously resolved process specification.
   */
  attach(socket: WebSocket, spec: IdeLanguageSpec): void {
    this.options.assertUsable()
    if (this.closing || this.connections.size >= this.options.limits.maxConnections) {
      socket.close(1013, 'Editor language capacity reached')
      return
    }
    const limits = this.options.limits
    const child = this.options.subprocess.spawn({ argv: spec.argv, cwd: spec.root,
      stdio: { stdin: 'pipe', stdout: 'pipe', stderr: { maxBytes: limits.maxStderrBytes } },
      graceMs: limits.killGraceMs, env: { ...spec.environment, PYTHONDONTWRITEBYTECODE: '1' },
    })
    const connection: Connection = { socket, child, closing: false }
    this.connections.add(connection)
    const stop = (): void => {
      void this.closeConnection(connection).catch((error: unknown) => { console.error('Editor language shutdown failed:', error) })
    }
    const isClosing = (): boolean => connection.closing
    const fail = (error: unknown): void => {
      if (connection.closing) return
      if (socket.readyState === 1) socket.send(JSON.stringify({ jsonrpc: '2.0', method: 'window/logMessage', params: {
        type: 1, message: error instanceof Error ? error.message : 'Editor language connection failed',
      } }))
      socket.close(1011, 'Editor language connection failed')
      stop()
    }
    socket.once('close', stop)
    socket.once('error', fail)
    const decoder = new MessageDecoder(limits.maxMessageBytes)
    child.stdout?.on('data', (chunk: Buffer) => {
      if (connection.closing) return
      try {
        for (const message of decoder.push(chunk)) {
          if (socket.bufferedAmount > limits.maxQueuedBytes) throw new Error('Editor language output is backpressured')
          if (socket.readyState === 1) socket.send(JSON.stringify(message))
        }
      } catch (error) { fail(error) }
    })
    let queuedBytes = 0
    let inbound: Promise<void> = Promise.resolve()
    socket.on('message', (data: RawData, binary: boolean) => {
      if (connection.closing) return
      const bytes = Array.isArray(data) ? Buffer.concat(data) : data instanceof ArrayBuffer ? Buffer.from(data) : data
      if (binary || bytes.length > limits.maxMessageBytes || queuedBytes + bytes.length > limits.maxQueuedBytes) {
        fail(new Error('Editor language input exceeds its message budget'))
        return
      }
      queuedBytes += bytes.length
      inbound = inbound.then(async () => {
        if (isClosing()) return
        const message: unknown = JSON.parse(bytes.toString('utf8'))
        const admitted = await prepareIdeLanguageMessage(message, spec)
        if (isClosing()) return
        const input = child.stdin
        if (!input || input.destroyed) throw new Error('Language server input is closed')
        const framed = encodeMessage(admitted)
        if (input.writableLength + framed.length > limits.maxQueuedBytes) throw new Error('Language server input is backpressured')
        await new Promise<void>((accept, reject) => { input.write(framed, (error) => { if (error) reject(error); else accept() }) })
      }).catch(fail).finally(() => { queuedBytes -= bytes.length })
    })
    void child.done.then((outcome) => {
      if (!connection.closing) socket.close(outcome.exitCode === 0 ? 1000 : 1011, outcome.exitCode === 0 ? 'Language server exited' : 'Language server stopped')
      stop()
    }, fail)
  }

  private closeConnection(connection: Connection): Promise<void> {
    if (connection.closePromise) return connection.closePromise
    connection.closing = true
    connection.closePromise = (async () => {
      connection.socket.removeAllListeners('message')
      connection.child.stdout?.removeAllListeners('data')
      connection.socket.close()
      connection.child.terminate()
      const quiet = await connection.child.waitForExit(AbortSignal.timeout(this.options.limits.shutdownMs))
      this.connections.delete(connection)
      if (!quiet) throw new Error('Editor language process did not stop within its shutdown budget')
    })()
    return connection.closePromise
  }

  /** Stop registrations' owned children and await their exit.
   * @returns completion after every admitted language process has stopped.
   */
  async close(): Promise<void> {
    this.closing = true
    const results = await Promise.allSettled([...this.connections].map(connection => this.closeConnection(connection)))
    const failures = results.filter((result): result is PromiseRejectedResult => result.status === 'rejected')
    if (failures.length) throw new AggregateError(failures.map((result): unknown => result.reason), 'Editor language shutdown failed')
  }
}
