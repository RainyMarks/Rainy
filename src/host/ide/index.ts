/** Human IDE Host composition: authenticated files, recovery, language services, run and debug. */
import type { IncomingMessage, ServerResponse } from 'node:http'
import { fileURLToPath } from 'node:url'
import type { Context } from '@deepseek-ai/cordis'
import s from '@deepseek-ai/schemastery'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type {} from '@deepseek-ai/dsh-fs'
import type {} from '@deepseek-ai/dsh-subprocess'
import type {} from '@deepseek-ai/dsh-storage-domain'
import type {} from '@deepseek-ai/dsh-host-webserver'
import type {} from '@deepseek-ai/dsh-client-connection'
import { WebSocketServer } from 'ws'
import { z } from 'zod'
import { RainyIdeFiles, ideFilesConfigSchema, parseIdeFilesRequest } from './ide-files.ts'
import { RainyIdeStateStore, ideStateConfigSchema, ideStateSpec, parseIdeStateRequest } from './ide-state.ts'
import { ideFailure, IdeOperationError } from './ide-files-core.ts'
import { createIdeExecutionService, ideExecutionFailure } from './ide-execution.ts'
import { ideExecutionLimitsSchema } from './ide-execution-schema.ts'
import { createIdeAssetHandler } from './ide-assets.ts'
import { IdeLanguageService } from './ide-language.ts'
import type { IdeLanguageLimits } from './ide-language.ts'
import { formatIdeDocument, ideLanguageSchema } from './ide-format.ts'
import type { IdeFormatLimits } from './ide-format.ts'
import { getIdeToolPaths, inspectIdeTools } from './ide-tools.ts'
import type {} from './project-roots.ts'
import type {} from './runtime.ts'
import { readRequestBytes } from './request-body.ts'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { IdeRootId } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'

export const name = 'rainy-ide'
export const inject = ['webServer', 'connection', 'workspaceRegistry', 'storageDomain', 'fs', 'subprocess', 'rainyProjectRoots', 'rainyRuntime']

/** Deployment budgets for editor services; delegated blocks are validated by their owning modules. */
export interface Config {
  /** Maximum complete JSON request, including escaped text and recovery metadata. */
  maxRequestBytes: number
  /** Whether startup may create a blank chat before the first user message. */
  startupSession: 'create' | 'restore-only'
  /** File-operation config, parsed by ideFilesConfigSchema before registration. */
  files: unknown
  /** Recovery config, parsed by ideStateConfigSchema before opening its domain. */
  state: unknown
  /** Run and debug config, parsed by ideExecutionLimitsSchema before process ownership begins. */
  execution: unknown
  /** Language process and transport limits. */
  language: IdeLanguageLimits
  /** Formatting limits; this service never writes the original document. */
  formatter: IdeFormatLimits
}

/** Config blocks are explicit deployment inputs; their feature owners reject unknown fields. */
export const Config: s<Config> = s.object({
  maxRequestBytes: s.number().min(1024).max(128 * 1024 * 1024).step(1).default(40 * 1024 * 1024),
  startupSession: s.union(['create', 'restore-only']).default('restore-only'),
  files: s.any().default({}), state: s.any().default({}), execution: s.any().default({}),
  language: s.object({
    maxConnections: s.number().min(1).max(64).step(1).default(8),
    maxMessageBytes: s.number().min(1024).max(64 * 1024 * 1024).step(1).default(16 * 1024 * 1024),
    maxQueuedBytes: s.number().min(1024).max(128 * 1024 * 1024).step(1).default(32 * 1024 * 1024),
    maxStderrBytes: s.number().min(1024).max(4 * 1024 * 1024).step(1).default(256 * 1024),
    killGraceMs: s.number().min(100).max(60000).step(1).default(2000),
    shutdownMs: s.number().min(1000).max(120000).step(1).default(10000),
  }).default({}),
  formatter: s.object({
    maxTextBytes: s.number().min(1024).max(32 * 1024 * 1024).step(1).default(5 * 1024 * 1024),
    maxStderrBytes: s.number().min(1024).max(4 * 1024 * 1024).step(1).default(65536),
    timeoutMs: s.number().min(1000).max(120000).step(1).default(15000),
    killGraceMs: s.number().min(100).max(60000).step(1).default(2000),
  }).default({}),
})

const languageQuery = z.object({ workspaceId: z.string().min(1).max(512).transform(WorkspaceId), language: ideLanguageSchema,
  rootId: z.string().min(1).max(128).transform(value => brandString<IdeRootId>(value)).optional() }).strict()

/** Decode the complete request under its byte budget before dispatch.
 * @param request - authenticated incoming request.
 * @param maxBytes - configured maximum complete request size.
 * @returns untrusted JSON for the operation-specific parser.
 */
export async function readIdeRequest(request: IncomingMessage, maxBytes: number): Promise<unknown> {
  const body = await readRequestBytes(request, maxBytes,
    () => new IdeOperationError('too-large', 'The IDE request exceeds its configured byte limit'))
  try { return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(body)) }
  catch (_invalidJson) { throw new IdeOperationError('invalid-request', 'The IDE request must contain valid UTF-8 JSON') }
}

function send(response: ServerResponse, status: number, value: unknown): void {
  if (response.destroyed) return
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' })
  response.end(JSON.stringify(value))
}

/** Mount the complete human IDE without adding Agent tools or Session events.
 * @param ctx - authenticated Host with workspace, file, process and storage providers.
 * @param config - resolved deployment budgets.
 */
export async function apply(ctx: Context, config: Config): Promise<void> {
  ctx.on('webserver/index-inject', (table) => {
    table.push({ kind: 'global', name: '__DSH_WORKSPACE_NAVIGATION_CONFIG__', value: { startupSession: config.startupSession } })
  })
  const filesConfig = ideFilesConfigSchema.parse(config.files)
  const stateConfig = ideStateConfigSchema.parse(config.state)
  const executionConfig = ideExecutionLimitsSchema.parse(config.execution)
  const tools = getIdeToolPaths()
  const files = new RainyIdeFiles({ registry: ctx.workspaceRegistry, roots: ctx.rainyProjectRoots, fs: ctx.fs, config: filesConfig })
  const state = new RainyIdeStateStore({ registry: ctx.workspaceRegistry, config: stateConfig,
    domain: await ctx.storageDomain.open(ideStateSpec) })
  ctx.provide('rainyIdeState', {
    getSelection: () => state.getSelection(),
    setSelection: (workspaceId: WorkspaceId | null) => state.setSelection(workspaceId),
  })
  const resolveWorkspace = (id: WorkspaceId, rootId?: IdeRootId) => files.resolveWorkspace(id, rootId)
  const execution = createIdeExecutionService({ subprocess: ctx.subprocess, resolveWorkspace, resources: tools,
    assertUsable: () => { ctx.rainyRuntime.assertCanStart() },
    resolveEnvironment: id => ctx.rainyRuntime.resolveWorkspace(id),
    limits: executionConfig, reportError: (error) => { console.error('IDE execution cleanup failed:', error) } })
  ctx.effect(() => ctx.rainyRuntime.registerActivitySource(() => execution.hasActivity()))
  const language = new IdeLanguageService({ subprocess: ctx.subprocess, resolveWorkspace,
    assertUsable: () => { ctx.rainyRuntime.assertCanStart() },
    resolveEnvironment: id => ctx.rainyRuntime.resolveWorkspace(id),
    configuration: id => state.get(id).data.execution, tools, limits: config.language })
  const sockets = new WebSocketServer({ noServer: true, maxPayload: config.language.maxMessageBytes, perMessageDeflate: false })
  let closing = false
  const isClosing = (): boolean => closing
  ctx.effect(() => async () => {
    closing = true
    const results = await Promise.allSettled([language.close(), execution.dispose(), files.close(), state.close()])
    for (const socket of sockets.clients) socket.terminate()
    await new Promise<void>((accept, reject) => { sockets.close((error) => { if (error) reject(error); else accept() }) })
    const failures = results.filter((result): result is PromiseRejectedResult => result.status === 'rejected')
    if (failures.length) throw new AggregateError(failures.map((result): unknown => result.reason), 'IDE shutdown did not complete')
  })
  const admit = (request: IncomingMessage): 401 | 403 | undefined => {
    const admission = ctx.connection.admit(request)
    return 'rejection' in admission ? admission.rejection : undefined
  }
  ctx.effect(() => ctx.webServer.register({ kind: 'prefix', path: '/rainy/editor', handler: createIdeAssetHandler({
    root: fileURLToPath(new URL('../resources/editor', import.meta.url)), admit,
  }) }))
  ctx.effect(() => ctx.webServer.register({ kind: 'exact', path: '/rainy/ide', handler: async (request, response) => {
    const rejection = admit(request)
    if (rejection !== undefined) { response.writeHead(rejection); response.end(); return }
    if (request.method !== 'POST') { response.writeHead(405); response.end(); return }
    if (closing) { send(response, 503, { ok: false, error: { code: 'closed', message: 'The IDE is closing' } }); return }
    const abort = new AbortController()
    const disconnected = (): void => { if (!response.writableEnded) abort.abort() }
    response.once('close', disconnected)
    let executionRequest = false
    try {
      const body = await readIdeRequest(request, config.maxRequestBytes)
      if (body === null || typeof body !== 'object' || !('op' in body) || typeof body.op !== 'string') throw new IdeOperationError('invalid-request', 'The IDE operation is missing')
      let value: unknown
      if (body.op.startsWith('workspaces.') || body.op.startsWith('files.')) value = await files.handle(parseIdeFilesRequest(body), abort.signal)
      else if (body.op.startsWith('state.')) value = await state.handle(parseIdeStateRequest(body))
      else if (body.op === 'tools.status') {
        z.object({ op: z.literal('tools.status') }).strict().parse(body)
        ctx.rainyRuntime.assertCanStart()
        value = await inspectIdeTools(ctx.subprocess, tools)
      } else if (body.op === 'format') {
        ctx.rainyRuntime.assertCanStart()
        value = await formatIdeDocument(body, {
          subprocess: ctx.subprocess, resolveWorkspace, tools, limits: config.formatter, signal: abort.signal,
        })
      }
      else {
        executionRequest = true
        value = await execution.handle(body)
      }
      send(response, 200, { ok: true, value })
    } catch (error) {
      const failure = executionRequest ? ideExecutionFailure(error) : error instanceof z.ZodError
        ? { code: 'invalid-request', message: 'The IDE request contains invalid fields' } : ideFailure(error)
      send(response, failure.code === 'too-large' ? 413 : failure.code === 'version-conflict' || failure.code === 'revision-conflict' ? 409 : 400, { ok: false, error: failure })
    } finally { response.removeListener('close', disconnected) }
  } }))
  ctx.effect(() => ctx.webServer.registerUpgrade({ path: '/rainy/ide/lsp', handler: async (request, socket, head) => {
    const rejection = admit(request)
    if (rejection !== undefined || closing) { socket.end(`HTTP/1.1 ${rejection ?? 503} Rejected\r\nConnection: close\r\n\r\n`); return }
    try {
      const url = new URL(request.url ?? '', 'http://localhost')
      const query = languageQuery.parse(Object.fromEntries(url.searchParams))
      const spec = await language.resolve(query.workspaceId, query.language, query.rootId)
      if (isClosing() || socket.destroyed) { socket.destroy(); return }
      sockets.handleUpgrade(request, socket, head, (client) => {
        try { language.attach(client, spec) }
        catch (error) { client.close(1011, 'Language server could not start'); console.error('IDE language startup failed:', error) }
      })
    } catch (_invalidLanguageRequest) { if (!socket.destroyed) socket.end('HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n') }
  } }))
}
