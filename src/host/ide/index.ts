/** The human IDE: project files, editor recovery, formatting, language servers, runs, terminals and debugging. */
import { join } from 'node:path'
import { z } from 'zod'
import type { Config } from '../../shared/config.ts'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { Activity } from '../activity.ts'
import type { HostEnvironment } from '../env.ts'
import type { Projects } from '../projects.ts'
import { RpcError } from '../rpc.ts'
import type { RuntimeService } from '../runtime/index.ts'
import type { HostServer } from '../server.ts'
import { createIdeExecutionService, ideExecutionFailure } from './execution.ts'
import { localIdeSubprocess } from './execution-process.ts'
import { ideExecutionLimitsSchema } from './execution-schema.ts'
import { ideFailure, IdeOperationError } from './files-core.ts'
import { parseIdeFilesRequest, RainyIdeFiles, resolveIdeFilesConfig } from './files.ts'
import { formatIdeDocument, type IdeFormatLimits } from './format.ts'
import { IdeLanguageService, type IdeLanguageLimits } from './language.ts'
import { parseIdeStateRequest, RainyIdeStateStore, resolveIdeStateConfig } from './state.ts'
import { getIdeToolPaths, inspectIdeTools } from './tools.ts'

/** Largest complete IDE request, including escaped file text and recovery buffers. */
export const MAX_IDE_REQUEST_BYTES = 40 * 1024 * 1024
/** Language-server connection limits. */
export const IDE_LANGUAGE_LIMITS: IdeLanguageLimits = {
  maxConnections: 8, maxMessageBytes: 16 * 1024 * 1024, maxQueuedBytes: 32 * 1024 * 1024, killGraceMs: 2000, shutdownMs: 10000,
}
/** Formatter limits. */
export const IDE_FORMAT_LIMITS: IdeFormatLimits = { maxTextBytes: 5 * 1024 * 1024, maxStderrBytes: 65536, timeoutMs: 15000, killGraceMs: 2000 }

/** The IDE as used by the Host entry. */
export interface IdeService {
  /**
   * Validate and run one operation of the `ide` RPC method.
   * @param request Untrusted request object with an `op` field.
   * @returns The operation result.
   * @throws RpcError with the IDE failure code, plus `currentVersion`/`currentState` for conflicts.
   */
  handle(request: unknown): Promise<unknown>
  /** @returns The remembered selected project, if it is still registered. */
  selection(): WorkspaceId | null
  /** @param id Project to remember as selected. */
  setSelection(id: WorkspaceId): Promise<void>
  /** Stop runs, debug sessions, terminals and language servers, and drain pending writes. */
  close(): Promise<void>
}

/** Dependencies of {@link createIde}. */
export interface IdeDependencies {
  readonly env: HostEnvironment
  readonly config: Config
  readonly projects: Projects
  readonly runtime: RuntimeService
  readonly server: HostServer
  readonly activity: Activity
  readonly log: (message: string) => void
}

function toRpcError(error: unknown): RpcError {
  if (error instanceof z.ZodError) return new RpcError('invalid-request', 'The IDE request contains invalid fields')
  const failure = ideFailure(error)
  const data = {
    ...failure.currentVersion === undefined ? {} : { currentVersion: failure.currentVersion },
    ...failure.currentState === undefined ? {} : { currentState: failure.currentState },
  }
  return new RpcError(failure.code, failure.message, Object.keys(data).length === 0 ? undefined : data)
}

/**
 * Build the IDE and register the `/rainy/ide/lsp` WebSocket.
 * @param deps Host environment, workbench config, project catalog, runtime selections, server and activity gate.
 * @returns The IDE service.
 */
export async function createIde(deps: IdeDependencies): Promise<IdeService> {
  const { env, config, projects, runtime, server, activity, log } = deps
  const tools = await getIdeToolPaths(env.resources)
  const files = new RainyIdeFiles({ projects, config: resolveIdeFilesConfig() })
  const state = await RainyIdeStateStore.open({ directory: join(env.home, 'ide'), projects, config: resolveIdeStateConfig(), log })
  const assertUsable = (): void => { activity.assertCanStart() }
  const resolveWorkspace = (id: WorkspaceId, rootId?: Parameters<RainyIdeFiles['resolveWorkspace']>[1]) => files.resolveWorkspace(id, rootId)
  const resolveEnvironment = (id: WorkspaceId) => runtime.resolveWorkspace(id)
  const execution = createIdeExecutionService({
    subprocess: localIdeSubprocess, pwshPath: env.pwshPath, resolveWorkspace, resources: tools, assertUsable, resolveEnvironment,
    limits: ideExecutionLimitsSchema.parse({ defaultCols: config.editorTerminalCols, defaultRows: config.editorTerminalRows }),
    reportError: (error) => { log(`IDE execution cleanup failed: ${error instanceof Error ? error.stack ?? error.message : String(error)}`) },
  })
  const removeActivity = activity.addSource(() => execution.hasActivity())
  const language = new IdeLanguageService({
    subprocess: localIdeSubprocess, resolveWorkspace, resolveEnvironment, assertUsable, tools, log, limits: IDE_LANGUAGE_LIMITS,
    configuration: async id => (await state.get(id)).data.execution,
  })
  const lifetime = new AbortController()
  let closing: Promise<void> | undefined

  server.socket('/rainy/ide/lsp', (socket, _request, url) => {
    if (closing !== undefined) { socket.close(1013, 'The IDE is closing'); return }
    language.accept(socket, url)
  })

  async function removeWorkspace(request: unknown): Promise<unknown> {
    const parsed = parseIdeFilesRequest(request)
    if (parsed.op !== 'workspaces.remove') throw new IdeOperationError('invalid-request', 'The IDE file request contains invalid fields.')
    const selected = state.getSelection().workspaceId === parsed.workspaceId
    await execution.stopWorkspace(parsed.workspaceId)
    await language.closeWorkspace(parsed.workspaceId)
    const result = await files.handle(parsed, lifetime.signal)
    if (selected) await state.setSelection(null)
    return result
  }

  async function dispatch(request: unknown): Promise<unknown> {
    if (request === null || typeof request !== 'object' || !('op' in request) || typeof request.op !== 'string') {
      throw new IdeOperationError('invalid-request', 'The IDE operation is missing')
    }
    const op = request.op
    if (op === 'workspaces.remove') return removeWorkspace(request)
    if (op.startsWith('workspaces.') || op.startsWith('files.') || op.startsWith('directories.')) return files.handle(parseIdeFilesRequest(request), lifetime.signal)
    if (op.startsWith('state.')) return state.handle(parseIdeStateRequest(request))
    if (op === 'tools.status') {
      z.object({ op: z.literal('tools.status') }).strict().parse(request)
      assertUsable()
      return inspectIdeTools(localIdeSubprocess, tools)
    }
    if (op === 'format') {
      assertUsable()
      return formatIdeDocument(request, { subprocess: localIdeSubprocess, resolveWorkspace, tools, limits: IDE_FORMAT_LIMITS, signal: lifetime.signal })
    }
    try {
      return await execution.handle(request)
    } catch (error) {
      const failure = ideExecutionFailure(error)
      throw new RpcError(failure.code, failure.message)
    }
  }

  return {
    async handle(request) {
      if (closing !== undefined) throw new RpcError('closed', 'The IDE is closing')
      try {
        if (Buffer.byteLength(JSON.stringify(request) ?? '') > MAX_IDE_REQUEST_BYTES) {
          throw new IdeOperationError('too-large', 'The IDE request exceeds its configured byte limit')
        }
        return await dispatch(request)
      } catch (error) {
        if (error instanceof RpcError) throw error
        throw toRpcError(error)
      }
    },
    selection: () => state.getSelection().workspaceId,
    async setSelection(id) { await state.setSelection(id) },
    close() {
      closing ??= (async () => {
        lifetime.abort()
        const results = await Promise.allSettled([language.close(), execution.dispose(), files.close(), state.close()])
        removeActivity()
        const failures = results.filter((result): result is PromiseRejectedResult => result.status === 'rejected')
        if (failures.length) throw new AggregateError(failures.map((result): unknown => result.reason), 'IDE shutdown did not complete')
      })()
      return closing
    },
  }
}
