/** Project-scoped human terminals, runs and launch-only debug sessions. */
import { randomUUID } from 'node:crypto'
import { z } from 'zod'
import { assertNever, brandString } from '../../shared/brand.ts'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type {
  IdeDebugId,
  IdeExecutionEvent,
  IdeExecutionRequest,
  IdeExecutionResponse,
  IdeExecutionStatus,
  IdeOperationId,
  IdeRunId,
  IdeRunSnapshot,
  IdeTerminalId,
  IdeTerminalSnapshot,
} from '../../shared/ide-execution-protocol.ts'
import { ExecutableNotFoundError, type ProcessHandle, type ProcessOutcome, type TerminalHandle } from '../process.ts'
import type { ResolvedWorkspaceEnvironment } from '../runtime/environments.ts'
import { IdeDebugSession } from './debug.ts'
import { IdeProcessOwner, writeIdeInput, type IdeOutputStream, type IdeSubprocess } from './execution-process.ts'
import {
  absoluteIdePath,
  nodeIdeExecutionFiles,
  resolveIdeExecutable,
  resolveIdeRun,
  resolveIdeWorkspacePath,
  type IdeExecutionFiles,
  type IdeExecutionResources,
  type IdeExecutionWorkspace,
  type ResolvedIdeRun,
} from './execution-resolve.ts'
import { ideExecutionRequestSchema, type IdeExecutionLimits } from './execution-schema.ts'
import { resolvePwshPath } from './tools.ts'

/** Dependencies of {@link createIdeExecutionService}. */
export interface IdeExecutionServiceOptions {
  readonly subprocess: IdeSubprocess
  /** PowerShell 7 shipped with the Windows Host; the IDE terminal uses it on Windows. */
  readonly pwshPath?: string | undefined
  readonly resolveWorkspace: (id: WorkspaceId, rootId?: IdeRootId) => Promise<IdeExecutionWorkspace>
  readonly resolveEnvironment?: (id: WorkspaceId) => ResolvedWorkspaceEnvironment
  /** Reject execution and input while the owning Host is switching targets. */
  readonly assertUsable: () => void
  readonly resources: IdeExecutionResources
  readonly limits: IdeExecutionLimits
  readonly reportError: (error: unknown) => void
  readonly files?: IdeExecutionFiles
}

/** Execution operations of the `ide` RPC method and their teardown. */
export interface IdeExecutionService {
  /** @returns whether any run, terminal or debug session still owns processes. */
  hasActivity(): boolean
  /** Dispatch one human IDE operation. @param input - untrusted request JSON. @returns its operation-specific value. */
  handle(input: unknown): Promise<IdeExecutionResponse>
  /** Stop every operation of one project, for example before the project is removed. @param workspaceId - project identity. @returns completion after their processes stopped. */
  stopWorkspace(workspaceId: WorkspaceId): Promise<void>
  /** Stop admission and await all owned operations. @returns quiescent cleanup. */
  dispose(): Promise<void>
}

type EventValue = IdeExecutionEvent extends infer E ? (E extends IdeExecutionEvent ? Omit<E, 'sequence'> : never) : never
interface BaseOperation {
  readonly id: IdeOperationId
  readonly workspaceId: WorkspaceId
  readonly owner: IdeProcessOwner
  task: Promise<void>
  stopped: boolean
  finished: boolean
  ownsBuildDirectory?: boolean
  terminal?: TerminalHandle
  process?: ProcessHandle
}
interface RunOperation extends BaseOperation {
  readonly kind: 'run'
  snapshot: IdeRunSnapshot
}
interface TerminalOperation extends BaseOperation {
  readonly kind: 'terminal'
  snapshot: IdeTerminalSnapshot
}
interface DebugOperation extends BaseOperation {
  readonly kind: 'debug'
  session: IdeDebugSession
}
type Operation = RunOperation | TerminalOperation | DebugOperation

function errorOf(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error))
}
function exitOf(outcome: ProcessOutcome, stopped: boolean): { exitCode: number | null; signal: string | null; stopped: boolean } {
  return { exitCode: outcome.exitCode, signal: outcome.signal, stopped }
}

/**
 * Convert route failures to the product's structured error fields.
 * @param error - admission, dependency or process failure.
 * @returns stable error code and message.
 */
export function ideExecutionFailure(error: unknown): { readonly code: string; readonly message: string } {
  if (error instanceof z.ZodError) return { code: 'invalid-request', message: error.issues.map(issue => issue.message).join(' ') }
  if (error instanceof ExecutableNotFoundError) return { code: 'dependency-unavailable', message: error.message }
  if (error instanceof Error && 'code' in error && (error.code === 'ENOENT' || error.code === 'EXECUTABLE_NOT_FOUND'))
    return { code: 'dependency-unavailable', message: error.message }
  return { code: 'execution-error', message: errorOf(error).message }
}

/**
 * Construct independently owned workspace operations and bounded immutable event history.
 * @param options - providers, resources and resolved deployment limits.
 * @returns the human execution service.
 */
export function createIdeExecutionService(options: IdeExecutionServiceOptions): IdeExecutionService {
  const files = options.files ?? nodeIdeExecutionFiles
  const resolver = {
    subprocess: options.subprocess,
    resources: options.resources,
    files,
    resolveWorkspace: options.resolveWorkspace,
    resolveEnvironment: options.resolveEnvironment,
    maxConfigurationBytes: options.limits.maxConfigurationBytes,
  }
  const operations = new Map<IdeOperationId, Operation>()
  const requests = new Set<Promise<IdeExecutionResponse>>()
  const history: { event: IdeExecutionEvent; bytes: number }[] = []
  let historyBytes = 0
  let sequence = 0
  let removedThrough = 0
  let disposed = false
  let disposal: Promise<void> | undefined

  function publish(value: EventValue): void {
    if (disposed) return
    const event = { ...value, sequence: ++sequence } as IdeExecutionEvent
    const bytes = Buffer.byteLength(JSON.stringify(event))
    if (bytes > options.limits.maxEventBytes) {
      removedThrough = event.sequence
      return
    }
    history.push({ event, bytes })
    historyBytes += bytes
    while (historyBytes > options.limits.maxOutputBytes) {
      const removed = history.shift()
      if (removed === undefined) throw new Error('IDE output history is inconsistent with its retained byte count.')
      historyBytes -= removed.bytes
      removedThrough = Math.max(removedThrough, removed.event.sequence)
    }
  }

  function output(workspaceId: WorkspaceId, operationId: IdeOperationId, stream: IdeOutputStream, text: string): void {
    if (disposed) return
    let offset = 0
    while (offset < text.length) {
      let low = 1
      let high = Math.min(text.length - offset, options.limits.maxEventBytes)
      while (low < high) {
        const middle = Math.ceil((low + high) / 2)
        const bytes = Buffer.byteLength(
          JSON.stringify({
            sequence: sequence + 1,
            workspaceId,
            kind: 'output',
            operationId,
            stream,
            text: text.slice(offset, offset + middle),
          }),
        )
        if (bytes <= options.limits.maxEventBytes) low = middle
        else high = middle - 1
      }
      if (low < text.length - offset && /[\uD800-\uDBFF]/.test(text[offset + low - 1])) low--
      if (low === 0) throw new Error('The configured IDE event limit cannot contain one output character.')
      publish({ workspaceId, kind: 'output', operationId, stream, text: text.slice(offset, offset + low) })
      offset += low
    }
  }

  function status(workspaceId: WorkspaceId): IdeExecutionStatus {
    const values = [...operations.values()].filter(operation => operation.workspaceId === workspaceId)
    return {
      runs: values.flatMap(operation => (operation.kind === 'run' ? [operation.snapshot] : [])),
      terminals: values.flatMap(operation => (operation.kind === 'terminal' ? [operation.snapshot] : [])),
      debugSessions: values.flatMap(operation => (operation.kind === 'debug' ? [operation.session.snapshot] : [])),
    }
  }

  function reserve(): void {
    if (disposed) throw new Error('The IDE execution service is closed.')
    if (operations.size < options.limits.maxOperations) return
    const finished = [...operations.values()].find(operation => operation.finished)
    if (!finished) throw new Error('The workspace execution limit is reached. Stop an operation before starting another.')
    operations.delete(finished.id)
  }

  function dimensions(cols: number, rows: number): void {
    if (cols > options.limits.maxTerminalDimension || rows > options.limits.maxTerminalDimension)
      throw new Error('The requested terminal dimensions exceed the configured limit.')
  }

  function operation(request: { workspaceId: WorkspaceId }, id: IdeOperationId): Operation {
    const value = operations.get(id)
    if (!value || value.workspaceId !== request.workspaceId) throw new Error('The operation does not belong to the selected workspace.')
    return value
  }

  function owner(workspaceId: WorkspaceId, id: IdeOperationId): IdeProcessOwner {
    return new IdeProcessOwner(options.subprocess, options.limits.killGraceMs, (stream, text) => {
      output(workspaceId, id, stream, text)
    })
  }

  async function build(value: BaseOperation, resolved: ResolvedIdeRun): Promise<ProcessOutcome | undefined> {
    value.owner.signal.throwIfAborted()
    if (resolved.ownedBuildDirectory) {
      await files.createBuildDirectory(resolved.spec.workspaceRoot, resolved.ownedBuildDirectory)
      value.ownsBuildDirectory = true
    }
    for (const command of resolved.spec.build) {
      value.owner.signal.throwIfAborted()
      const result = await value.owner.command(command)
      if (result.exitCode !== 0 || value.stopped) return result
    }
    if (resolved.spec.build.length) await resolveIdeWorkspacePath(files, resolved.spec.workspaceRoot, resolved.spec.launch.argv[0], 'file')
    value.owner.signal.throwIfAborted()
    return undefined
  }

  async function cleanupBuild(value: BaseOperation, resolved: ResolvedIdeRun): Promise<void> {
    if (value.ownsBuildDirectory && resolved.ownedBuildDirectory)
      await files.removeBuildDirectory(resolved.spec.workspaceRoot, resolved.ownedBuildDirectory)
  }

  function publishRun(value: RunOperation): void {
    publish({ kind: 'run', workspaceId: value.workspaceId, run: value.snapshot })
  }
  function publishTerminal(value: TerminalOperation): void {
    publish({ kind: 'terminal', workspaceId: value.workspaceId, terminal: value.snapshot })
  }

  async function executeRun(value: RunOperation, resolved: ResolvedIdeRun, cols: number, rows: number): Promise<void> {
    try {
      if (resolved.spec.build.length) {
        value.snapshot = { ...value.snapshot, phase: 'building' }
        publishRun(value)
      }
      const failure = await build(value, resolved)
      if (failure) {
        value.snapshot = {
          ...value.snapshot,
          phase: value.stopped ? 'exited' : 'failed',
          exit: exitOf(failure, value.stopped),
          error: value.stopped ? undefined : 'The build failed; the program was not launched.',
        }
        return
      }
      value.snapshot = { ...value.snapshot, phase: 'running' }
      publishRun(value)
      let result: ProcessOutcome
      if (resolved.spec.terminal) {
        value.terminal = await value.owner.terminal(resolved.spec.launch, cols, rows)
        result = await value.owner.waitTerminal(value.terminal, value.owner.pump(value.terminal.output, 'terminal'))
      } else {
        value.process = value.owner.spawn(resolved.spec.launch)
        result = await value.owner.waitProcess(value.process, [
          value.owner.pump(value.process.stdout, 'stdout'),
          value.owner.pump(value.process.stderr, 'stderr'),
        ])
      }
      value.snapshot = { ...value.snapshot, phase: 'exited', exit: exitOf(result, value.stopped) }
    } catch (error) {
      value.snapshot = {
        ...value.snapshot,
        phase: value.stopped ? 'exited' : 'failed',
        ...(value.stopped ? { exit: { exitCode: null, signal: null, stopped: true } } : { error: errorOf(error).message }),
      }
    } finally {
      // Each release step runs even when an earlier one fails, so an owned build directory is never left behind.
      for (const release of [() => value.owner.close(), () => cleanupBuild(value, resolved)]) {
        try { await release() }
        catch (error) {
          value.snapshot = { ...value.snapshot, phase: 'failed', error: errorOf(error).message }
          options.reportError(error)
        }
      }
      value.finished = true
      publishRun(value)
    }
  }

  async function startRun(request: Extract<IdeExecutionRequest, { op: 'run.start' }>): Promise<IdeRunSnapshot> {
    options.assertUsable()
    const resolved = await resolveIdeRun(resolver, request.workspaceId, request.configuration)
    const cols = request.cols ?? options.limits.defaultCols
    const rows = request.rows ?? options.limits.defaultRows
    dimensions(cols, rows)
    reserve()
    const id = brandString<IdeRunId>(randomUUID())
    const value: RunOperation = {
      id,
      workspaceId: request.workspaceId,
      kind: 'run',
      owner: owner(request.workspaceId, id),
      stopped: false,
      finished: false,
      task: Promise.resolve(),
      snapshot: { id, workspaceId: request.workspaceId, name: resolved.spec.name, phase: 'starting', spec: resolved.spec },
    }
    operations.set(id, value)
    publishRun(value)
    value.task = executeRun(value, resolved, cols, rows)
    return value.snapshot
  }

  async function startTerminal(request: Extract<IdeExecutionRequest, { op: 'terminal.start' }>): Promise<IdeTerminalSnapshot> {
    options.assertUsable()
    dimensions(request.cols, request.rows)
    const world = await options.subprocess.terminalEnvironment()
    const workspace = await options.resolveWorkspace(request.workspaceId, request.rootId)
    const root = absoluteIdePath(await files.realpath(absoluteIdePath(workspace.root)))
    const cwd = await resolveIdeWorkspacePath(files, root, request.cwd ?? '.', 'directory')
    const environment = options.resolveEnvironment?.(request.workspaceId).environment ?? {}
    const shell = await resolveIdeExecutable(options.subprocess,
      world.platform === 'windows' ? resolvePwshPath(options.pwshPath) : world.defaultShell,
      world.platform === 'windows' ? 'pwsh.exe' : '/bin/bash', environment)
    reserve()
    const id = brandString<IdeTerminalId>(randomUUID())
    const value: TerminalOperation = {
      id,
      workspaceId: request.workspaceId,
      kind: 'terminal',
      owner: owner(request.workspaceId, id),
      stopped: false,
      finished: false,
      task: Promise.resolve(),
      snapshot: { id, workspaceId: request.workspaceId, cwd, phase: 'starting' },
    }
    operations.set(id, value)
    publishTerminal(value)
    value.task = (async () => {
      try {
        value.terminal = await value.owner.terminal({ argv: [shell, ...(world.platform === 'windows' ? ['-NoLogo', '-NoProfile'] : ['--noprofile', '--norc', '-i'])], cwd, environment }, request.cols, request.rows)
        value.snapshot = { ...value.snapshot, phase: 'running' }
        publishTerminal(value)
        const result = await value.owner.waitTerminal(value.terminal, value.owner.pump(value.terminal.output, 'terminal'))
        value.snapshot = { ...value.snapshot, phase: 'exited', exit: exitOf(result, value.stopped) }
      } catch (error) {
        value.snapshot = {
          ...value.snapshot,
          phase: value.stopped ? 'exited' : 'failed',
          ...(value.stopped ? { exit: { exitCode: null, signal: null, stopped: true } } : { error: errorOf(error).message }),
        }
      } finally {
        try {
          await value.owner.close()
        } catch (error) {
          value.snapshot = { ...value.snapshot, phase: 'failed', error: errorOf(error).message }
          options.reportError(error)
        }
        value.finished = true
        publishTerminal(value)
      }
    })()
    return value.snapshot
  }

  async function startDebug(
    request: Extract<IdeExecutionRequest, { op: 'debug.start' }>,
  ): Promise<IdeExecutionResponse<Extract<IdeExecutionRequest, { op: 'debug.start' }>>> {
    options.assertUsable()
    if (request.configuration.language === 'php') throw new Error('PHP supports running and terminals; no PHP debug adapter is configured.')
    const resolved = await resolveIdeRun(resolver, request.workspaceId, request.configuration)
    const cols = request.cols ?? options.limits.defaultCols
    const rows = request.rows ?? options.limits.defaultRows
    dimensions(cols, rows)
    reserve()
    const id = brandString<IdeDebugId>(randomUUID())
    const processOwner = owner(request.workspaceId, id)
    const session = new IdeDebugSession({
      snapshot: {
        ...(resolved.configuration.rootId === undefined ? {} : { rootId: resolved.configuration.rootId }),
        id,
        workspaceId: request.workspaceId,
        name: resolved.spec.name,
        language: resolved.spec.language,
        phase: resolved.spec.build.length ? 'building' : 'starting',
        breakpoints: [],
      },
      resolved,
      resolver,
      subprocess: options.subprocess,
      owner: processOwner,
      limits: options.limits,
      cols,
      rows,
      stopOnEntry: request.stopOnEntry ?? false,
      breakpoints: request.breakpoints.filter(source => (source.rootId ?? 'primary') === (resolved.configuration.rootId ?? 'primary')),
      output: (stream, text) => {
        output(request.workspaceId, id, stream, text)
      },
      publish: (snapshot) => {
        publish({ kind: 'debug', workspaceId: request.workspaceId, debug: snapshot })
      },
      reportError: options.reportError,
    })
    const value: DebugOperation = {
      id,
      workspaceId: request.workspaceId,
      kind: 'debug',
      owner: processOwner,
      session,
      stopped: false,
      finished: false,
      task: Promise.resolve(),
    }
    operations.set(id, value)
    publish({ kind: 'debug', workspaceId: request.workspaceId, debug: session.snapshot })
    value.task = (async () => {
      try {
        const failure = await build(value, resolved)
        if (failure)
          throw new Error(
            value.stopped ? 'Debugging was stopped during the build.' : 'The debug build failed; the program was not launched.',
          )
        await session.start()
        await session.done
      } catch (error) {
        session.failStart(errorOf(error))
      } finally {
        for (const release of [() => session.stop(), () => cleanupBuild(value, resolved)]) {
          try { await release() }
          catch (error) { options.reportError(error) }
        }
        value.finished = true
      }
    })()
    return session.snapshot
  }

  async function stop(value: Operation): Promise<void> {
    if (value.finished) return
    value.stopped = true
    if (value.kind === 'debug') await value.session.stop()
    else {
      value.snapshot = { ...value.snapshot, phase: 'stopping' }
      if (value.kind === 'run') publishRun(value)
      else publishTerminal(value)
      await value.owner.close()
    }
    await value.task
  }

  async function dispatch(input: unknown): Promise<IdeExecutionResponse> {
    if (disposed) throw new Error('The IDE execution service is closed.')
    if (Buffer.byteLength(JSON.stringify(input)) > options.limits.maxConfigurationBytes)
      throw new Error('The IDE request exceeds the configured input limit.')
    const request = ideExecutionRequestSchema.parse(input)
    await options.resolveWorkspace(request.workspaceId)
    if ('cols' in request && request.cols !== undefined && 'rows' in request && request.rows !== undefined)
      dimensions(request.cols, request.rows)
    switch (request.op) {
      case 'execution.status':
        return status(request.workspaceId)
      case 'execution.poll':
        return {
          events: history
            .filter(item => item.event.workspaceId === request.workspaceId && item.event.sequence > request.cursor)
            .map(item => item.event),
          cursor: sequence,
          truncated: request.cursor < removedThrough,
          status: status(request.workspaceId),
        }
      case 'run.resolve':
        return (await resolveIdeRun(resolver, request.workspaceId, request.configuration)).spec
      case 'run.start':
        return startRun(request)
      case 'terminal.start':
        return startTerminal(request)
      case 'debug.start':
        return startDebug(request)
      case 'terminal.input':
      case 'terminal.resize':
      case 'terminal.stop': {
        const value = operation(request, request.terminalId)
        if (value.kind !== 'terminal') throw new Error('The requested terminal does not exist.')
        if (request.op === 'terminal.stop') {
          await stop(value)
          return { ok: true }
        }
        if (!value.terminal || value.finished || value.stopped) throw new Error('This terminal is not running.')
        if (request.op === 'terminal.input') {
          options.assertUsable()
          await value.terminal.write(request.data)
        }
        else await value.terminal.resize(request.cols, request.rows)
        return { ok: true }
      }
      case 'run.input':
      case 'run.resize':
      case 'run.stop': {
        const value = operation(request, request.runId)
        if (value.kind !== 'run') throw new Error('The requested run does not exist.')
        if (request.op === 'run.stop') {
          await stop(value)
          return { ok: true }
        }
        if (value.finished || value.stopped) throw new Error('This program is not running.')
        if (request.op === 'run.input') {
          options.assertUsable()
          if (value.terminal) await value.terminal.write(request.data)
          else await writeIdeInput(value.process?.stdin, request.data)
        } else {
          if (!value.terminal) throw new Error('This program does not use a terminal.')
          await value.terminal.resize(request.cols, request.rows)
        }
        return { ok: true }
      }
      case 'debug.setBreakpoints':
      case 'debug.threads':
      case 'debug.stack':
      case 'debug.scopes':
      case 'debug.variables':
      case 'debug.evaluate':
      case 'debug.control':
      case 'debug.input':
      case 'debug.resize':
      case 'debug.stop': {
        const value = operation(request, request.debugId)
        if (value.kind !== 'debug') throw new Error('The requested debug session does not exist.')
        if (request.op === 'debug.stop') {
          await stop(value)
          return { ok: true }
        }
        if (request.op === 'debug.input' || request.op === 'debug.evaluate') options.assertUsable()
        return value.session.handle(request)
      }
      default:
        return assertNever(request)
    }
  }

  return {
    hasActivity: () => [...operations.values()].some(operation => !operation.finished),
    async stopWorkspace(workspaceId) {
      const results = await Promise.allSettled([...operations.values()].filter(value => value.workspaceId === workspaceId).map(stop))
      for (const value of [...operations.values()]) if (value.workspaceId === workspaceId) operations.delete(value.id)
      const failures = results.flatMap(result => result.status === 'rejected' ? [result.reason as unknown] : [])
      if (failures.length) throw new AggregateError(failures, 'IDE operation cleanup failed.')
    },
    handle(input) {
      const task = dispatch(input)
      requests.add(task)
      void task
        .finally(() => {
          requests.delete(task)
        })
        .catch(() => {
          /* The route caller observes the request error. */
        })
      return task
    },
    dispose() {
      if (disposal) return disposal
      disposed = true
      disposal = (async () => {
        const stopping = Promise.allSettled([...operations.values()].map(stop))
        await Promise.allSettled([...requests])
        const results = await stopping
        const failures: unknown[] = []
        for (const result of results) {
          if (result.status === 'rejected') failures.push(result.reason)
        }
        operations.clear()
        history.length = 0
        historyBytes = 0
        if (failures.length) throw new AggregateError(failures, 'IDE operation cleanup failed.')
      })()
      return disposal
    },
  }
}
