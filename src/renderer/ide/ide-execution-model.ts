/** Retained workspace execution mirrors with bounded output, cursor ordering and stale-debug-read suppression. */
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import type { WorkspaceId } from '../ide-files-protocol.ts'
import type {
  IdeDebugEvaluation, IdeDebugFrame, IdeDebugId, IdeDebugScope, IdeDebugSnapshot, IdeDebugThread, IdeDebugVariable,
  IdeExecutionConfiguration, IdeExecutionPoll, IdeExecutionRequest, IdeExecutionResponseMap, IdeExecutionStatus,
  IdeOperationId, IdeRunConfiguration,
} from '../ide-execution-protocol.ts'
import type { IdeExecutionApi } from './ide-execution-api.ts'
import { fileKey, fileReference } from './ide-paths.ts'

/** Current selected workspace and the execution views rendered from its Host state. */
export interface IdeExecutionState {
  readonly workspaceId: WorkspaceId | null
  readonly status: IdeExecutionStatus
  readonly outputs: Readonly<Partial<Record<IdeOperationId, string>>>
  readonly selected: IdeOperationId | null
  readonly debugId: IdeDebugId | null
  readonly threads: readonly IdeDebugThread[]
  readonly frames: readonly IdeDebugFrame[]
  readonly scopes: readonly IdeDebugScope[]
  readonly variables: Readonly<Partial<Record<number, readonly IdeDebugVariable[]>>>
  readonly watches: Readonly<Partial<Record<string, IdeDebugEvaluation | string>>>
  readonly threadId: number | null
  readonly frameId: number | null
  readonly busy: boolean
  readonly truncated: boolean
  readonly error: string
}

/** Composition-owned timing, memory budgets and view/configuration callbacks. */
export interface IdeExecutionModelOptions {
  readonly pollMs: number
  readonly activePollMs: number
  /** Total retained output characters per workspace. */
  readonly maxOutputCharacters: number
  readonly maxRetainedWorkspaces: number
  readonly terminalCols: number
  readonly terminalRows: number
  readonly getConfiguration: () => IdeExecutionConfiguration
  readonly setConfiguration: (configuration: IdeExecutionConfiguration) => void
  readonly onError: (error: unknown) => void
  readonly onReveal: (path: string, line: number, column: number) => void
}

interface RetainedWorkspace {
  readonly workspaceId: WorkspaceId
  snapshot: IdeExecutionState
  cursor: number
  mutation: number
  pendingCommands: number
  debugEpoch: number
  pauseKey: string | undefined
  readonly pauseSequences: Map<IdeDebugId, number>
}

function emptyState(workspaceId: WorkspaceId | null): IdeExecutionState {
  return { workspaceId, status: { runs: [], terminals: [], debugSessions: [] }, outputs: {}, selected: null, debugId: null,
    threads: [], frames: [], scopes: [], variables: {}, watches: {}, threadId: null, frameId: null,
    busy: false, truncated: false, error: '' }
}

function active(status: IdeExecutionStatus): boolean {
  return status.terminals.some(value => value.phase !== 'exited' && value.phase !== 'failed')
    || status.runs.some(value => value.phase !== 'exited' && value.phase !== 'failed')
    || status.debugSessions.some(value => value.phase !== 'terminated' && value.phase !== 'failed')
}

function operationIds(status: IdeExecutionStatus): IdeOperationId[] {
  return [...status.terminals.map(value => value.id), ...status.runs.map(value => value.id), ...status.debugSessions.map(value => value.id)]
}

function messageOf(error: unknown): string { return error instanceof Error ? error.message : String(error) }

/** React-free execution owner; changing workspaces never stops the user's running processes. */
export class IdeExecutionModel {
  readonly state = createSnapshotStore<IdeExecutionState>(emptyState(null))
  private readonly workspaces = new Map<WorkspaceId, RetainedWorkspace>()
  private readonly requests = new Set<Promise<unknown>>()
  private readonly controllers = new Set<AbortController>()
  private timer: ReturnType<typeof setTimeout> | undefined
  private polling: { readonly generation: number; readonly controller: AbortController; readonly promise: Promise<void> } | undefined
  private generation = 0
  private disposed = false
  private disposal: Promise<void> | undefined
  private dimensions: { cols: number; rows: number }

  /** @param api - validated execution transport. @param options - resolved limits and composition callbacks. */
  constructor(private readonly api: IdeExecutionApi, private readonly options: IdeExecutionModelOptions) {
    this.dimensions = { cols: options.terminalCols, rows: options.terminalRows }
  }

  /**
   * Select a retained workspace and start readonly event polling.
   * @param workspaceId - selected project or null when no project is open.
   */
  setWorkspace(workspaceId: WorkspaceId | null): void {
    if (this.disposed || this.state.getSnapshot().workspaceId === workspaceId) return
    this.generation++
    this.clearTimer()
    this.polling?.controller.abort()
    this.polling = undefined
    if (workspaceId === null) { this.state.set(emptyState(null)); return }
    let record = this.workspaces.get(workspaceId)
    if (record === undefined) {
      record = { workspaceId, snapshot: emptyState(workspaceId), cursor: 0, mutation: 0, pendingCommands: 0,
        debugEpoch: 0, pauseKey: undefined, pauseSequences: new Map() }
    }
    this.workspaces.delete(workspaceId)
    this.workspaces.set(workspaceId, record)
    while (this.workspaces.size > this.options.maxRetainedWorkspaces) {
      const oldest = this.workspaces.keys().next().value
      if (oldest === undefined || oldest === workspaceId) break
      this.workspaces.delete(oldest)
    }
    this.state.set(record.snapshot)
    void this.refresh().catch((error: unknown) => { this.report(record, error) })
  }

  /** @param operationId - output or debug operation selected by the user. */
  select(operationId: IdeOperationId): void {
    const record = this.current()
    if (record === undefined || !operationIds(record.snapshot.status).includes(operationId)) return
    const debug = record.snapshot.status.debugSessions.find(value => value.id === operationId)
    this.patch(record, { selected: operationId, ...debug === undefined ? {} : { debugId: debug.id } })
    if (debug !== undefined) {
      record.pauseKey = undefined
      this.updateDebug(record)
    }
  }

  /** @returns a new interactive terminal in the selected workspace. */
  terminal(): Promise<void> {
    return this.command(async (record) => {
      const terminal = await this.request({ op: 'terminal.start', workspaceId: record.workspaceId, ...this.dimensions })
      this.patch(record, { status: { ...record.snapshot.status, terminals: [...record.snapshot.status.terminals, terminal] },
        selected: terminal.id })
    })
  }

  /** @param configuration - explicit human run choices. @returns completion of process admission and refresh. */
  run(configuration: IdeRunConfiguration): Promise<void> {
    if (this.current() === undefined) return Promise.resolve()
    this.remember(configuration)
    return this.command(async (record) => {
      const run = await this.request({ op: 'run.start', workspaceId: record.workspaceId, configuration, ...this.dimensions })
      this.patch(record, { status: { ...record.snapshot.status, runs: [...record.snapshot.status.runs, run] }, selected: run.id })
    })
  }

  /** @param configuration - explicit launch choices. @returns completion of debug launch admission and refresh. */
  debug(configuration: IdeRunConfiguration): Promise<void> {
    if (this.current() === undefined) return Promise.resolve()
    this.remember(configuration)
    const breakpoints = this.options.getConfiguration().breakpoints.filter(source =>
      (source.rootId ?? 'primary') === (configuration.rootId ?? 'primary'))
    return this.command(async (record) => {
      const debug = await this.request({ op: 'debug.start', workspaceId: record.workspaceId, configuration, breakpoints, ...this.dimensions })
      record.pauseKey = undefined
      this.patch(record, { status: { ...record.snapshot.status, debugSessions: [...record.snapshot.status.debugSessions, debug] },
        selected: debug.id, debugId: debug.id })
    })
  }

  /** @returns completion after the selected human operation receives its stop command. */
  stop(): Promise<void> {
    return this.command(async (record) => {
      const selected = record.snapshot.selected
      const terminal = record.snapshot.status.terminals.find(value => value.id === selected)
      const run = record.snapshot.status.runs.find(value => value.id === selected)
      const debug = record.snapshot.status.debugSessions.find(value => value.id === selected)
      if (terminal !== undefined) await this.request({ op: 'terminal.stop', workspaceId: record.workspaceId, terminalId: terminal.id })
      else if (run !== undefined) await this.request({ op: 'run.stop', workspaceId: record.workspaceId, runId: run.id })
      else if (debug !== undefined) await this.request({ op: 'debug.stop', workspaceId: record.workspaceId, debugId: debug.id })
    })
  }

  /** @param data - user keystrokes for the selected process. @returns completion of its input write. */
  async input(data: string): Promise<void> {
    const record = this.current()
    if (record === undefined || data === '') return
    const selected = record.snapshot.selected
    const terminal = record.snapshot.status.terminals.find(value => value.id === selected && value.phase === 'running')
    const run = record.snapshot.status.runs.find(value => value.id === selected && value.phase === 'running')
    const debug = record.snapshot.status.debugSessions.find(value => value.id === selected && (value.phase === 'running' || value.phase === 'paused'))
    if (terminal !== undefined) await this.request({ op: 'terminal.input', workspaceId: record.workspaceId, terminalId: terminal.id, data })
    else if (run !== undefined) await this.request({ op: 'run.input', workspaceId: record.workspaceId, runId: run.id, data })
    else if (debug !== undefined) await this.request({ op: 'debug.input', workspaceId: record.workspaceId, debugId: debug.id, data })
  }

  /** @param cols - measured character columns. @param rows - measured character rows. @returns completed PTY resize when one is active. */
  async resize(cols: number, rows: number): Promise<void> {
    this.dimensions = { cols, rows }
    const record = this.current()
    if (record === undefined) return
    const selected = record.snapshot.selected
    const terminal = record.snapshot.status.terminals.find(value => value.id === selected && value.phase === 'running')
    const run = record.snapshot.status.runs.find(value => value.id === selected && value.phase === 'running' && value.spec.terminal)
    const debug = record.snapshot.status.debugSessions.find(value => value.id === selected && (value.phase === 'running' || value.phase === 'paused'))
    if (terminal !== undefined) await this.request({ op: 'terminal.resize', workspaceId: record.workspaceId, terminalId: terminal.id, cols, rows })
    else if (run !== undefined) await this.request({ op: 'run.resize', workspaceId: record.workspaceId, runId: run.id, cols, rows })
    else if (debug !== undefined) await this.request({ op: 'debug.resize', workspaceId: record.workspaceId, debugId: debug.id, cols, rows })
  }

  /** @param action - supported human debug control. @returns completion followed by an immediate new status poll. */
  control(action: Extract<IdeExecutionRequest, { op: 'debug.control' }>['action']): Promise<void> {
    return this.command(async (record) => {
      const debug = this.debugSnapshot(record)
      if (debug === undefined) return
      record.debugEpoch++
      record.pauseKey = undefined
      let threadId = record.snapshot.threadId ?? debug.threadId
      if (threadId === undefined) threadId = (await this.request({ op: 'debug.threads', workspaceId: record.workspaceId, debugId: debug.id })).at(0)?.id
      if (threadId === undefined) return
      await this.request({ op: 'debug.control', workspaceId: record.workspaceId, debugId: debug.id, action, threadId })
      if (action !== 'pause') this.patch(record, { frames: [], scopes: [], variables: {}, watches: {}, frameId: null })
    })
  }

  /** @param threadId - stopped thread chosen by the user. @returns its current stack and initial frame details. */
  async selectThread(threadId: number): Promise<void> {
    const record = this.current()
    const debug = record === undefined ? undefined : this.debugSnapshot(record)
    if (record === undefined || debug?.phase !== 'paused') return
    const epoch = ++record.debugEpoch
    this.patch(record, { threadId, frames: [], scopes: [], variables: {}, frameId: null, watches: {} })
    await this.loadStack(record, debug.id, threadId, epoch)
  }

  /** @param frameId - stack frame selected by the user. @returns frame scopes, eager variables and watches. */
  async selectFrame(frameId: number): Promise<void> {
    const record = this.current()
    const debug = record === undefined ? undefined : this.debugSnapshot(record)
    if (record === undefined || debug?.phase !== 'paused') return
    await this.loadFrame(record, debug.id, frameId, ++record.debugEpoch)
  }

  /** @param variablesReference - adapter-issued child handle. @returns completion after publishing the children for the current pause. */
  async expandVariables(variablesReference: number): Promise<void> {
    const record = this.current()
    const debug = record === undefined ? undefined : this.debugSnapshot(record)
    if (record === undefined || debug?.phase !== 'paused' || variablesReference <= 0) return
    await this.loadVariables(record, debug.id, variablesReference, record.debugEpoch)
  }

  /** @param expression - explicit user expression. @param context - watch or console evaluation. @returns the adapter's evaluated value. */
  async evaluate(expression: string, context: 'watch' | 'repl'): Promise<IdeDebugEvaluation> {
    const record = this.current()
    const debug = record === undefined ? undefined : this.debugSnapshot(record)
    if (record === undefined || debug?.phase !== 'paused') throw new Error('The debugger is not paused in a selected workspace.')
    const epoch = record.debugEpoch
    try {
      const result = await this.evaluateInFrame(record, debug.id, expression, context)
      if (context === 'watch' && this.validDebug(record, debug.id, epoch)) {
        this.patch(record, { watches: { ...record.snapshot.watches, [expression]: result } })
      }
      return result
    } catch (error) {
      if (context === 'watch' && this.validDebug(record, debug.id, epoch)) {
        this.patch(record, { watches: { ...record.snapshot.watches, [expression]: messageOf(error) } })
      }
      throw error
    }
  }

  /**
   * @param path - workspace-relative source.
   * @param line - one-based line toggled by the user.
   * @returns completion of active-adapter replacement.
   */
  async toggleBreakpoint(path: string, line: number): Promise<void> {
    if (this.current() === undefined) return
    const configuration = this.options.getConfiguration()
    const reference = fileReference(path)
    const lines = new Set(configuration.breakpoints.find(source => fileKey(source.path, source.rootId) === path)?.lines ?? [])
    if (lines.has(line)) lines.delete(line); else lines.add(line)
    const source = { ...reference, lines: [...lines].sort((a, b) => a - b) }
    this.options.setConfiguration({ ...configuration,
      breakpoints: [...configuration.breakpoints.filter(value => fileKey(value.path, value.rootId) !== path),
        ...source.lines.length === 0 ? [] : [source]] })
    const record = this.current()
    if (record === undefined) return
    const debug = this.debugSnapshot(record)
    if (debug === undefined || debug.phase === 'terminated' || debug.phase === 'failed') return
    if ((debug.rootId ?? 'primary') !== (reference.rootId ?? 'primary')) return
    record.mutation++
    await this.request({ op: 'debug.setBreakpoints', workspaceId: record.workspaceId, debugId: debug.id, source })
    record.mutation++
    await this.refresh()
  }

  /** @returns a fresh selected-workspace poll, waiting for any earlier poll before issuing the new read. */
  async refresh(): Promise<void> {
    const generation = this.generation
    this.clearTimer()
    if (this.polling !== undefined && this.polling.generation === generation) await this.polling.promise
    if (this.disposed || generation !== this.generation) return
    this.clearTimer()
    await this.poll()
  }

  /** @returns resolution after outstanding client requests settle; the Host retains ownership of running operations. */
  dispose(): Promise<void> {
    this.disposed = true
    this.generation++
    this.clearTimer()
    for (const controller of this.controllers) controller.abort()
    this.polling?.controller.abort()
    this.disposal ??= Promise.allSettled([...this.requests]).then(() => { this.workspaces.clear() })
    return this.disposal
  }

  private current(): RetainedWorkspace | undefined {
    const workspaceId = this.state.getSnapshot().workspaceId
    return this.disposed || workspaceId === null ? undefined : this.workspaces.get(workspaceId)
  }

  private patch(record: RetainedWorkspace, patch: Partial<IdeExecutionState>): void {
    if (this.disposed || this.workspaces.get(record.workspaceId) !== record) return
    const unchanged = Object.keys(patch).every(key =>
      Object.is(record.snapshot[key as keyof IdeExecutionState], patch[key as keyof IdeExecutionState]))
    if (unchanged) return
    record.snapshot = { ...record.snapshot, ...patch }
    if (this.state.getSnapshot().workspaceId === record.workspaceId) this.state.set(record.snapshot)
  }

  private report(record: RetainedWorkspace, error: unknown): void {
    if (this.disposed || this.current() !== record || error instanceof Error && error.name === 'AbortError') return
    this.patch(record, { error: messageOf(error) })
    try { this.options.onError(error) }
    catch (listenerError) { console.error('IDE execution error listener failed', listenerError) }
  }

  private request<K extends IdeExecutionRequest['op']>(
    request: Extract<IdeExecutionRequest, { op: K }>, signal?: AbortSignal,
  ): Promise<IdeExecutionResponseMap[K]> {
    if (this.disposed) return Promise.reject(new Error('The workspace execution view is closed.'))
    const controller = new AbortController()
    const cancel = (): void => { controller.abort(signal?.reason) }
    if (signal?.aborted) cancel(); else signal?.addEventListener('abort', cancel, { once: true })
    this.controllers.add(controller)
    const promise = this.api.request(request, controller.signal)
    this.requests.add(promise)
    const settled = (): void => {
      this.requests.delete(promise)
      this.controllers.delete(controller)
      signal?.removeEventListener('abort', cancel)
    }
    void promise.then(settled, settled)
    return promise
  }

  private remember(configuration: IdeRunConfiguration): void {
    const current = this.options.getConfiguration()
    this.options.setConfiguration({ ...current,
      profiles: [...current.profiles.filter(profile => profile.name !== configuration.name), configuration],
      activeProfile: configuration.name })
  }

  private async command(action: (record: RetainedWorkspace) => Promise<void>): Promise<void> {
    const record = this.current()
    if (record === undefined) return
    record.mutation++
    record.pendingCommands++
    this.patch(record, { busy: true, error: '' })
    try { await action(record) }
    catch (error) { this.patch(record, { error: messageOf(error) }); throw error }
    finally {
      record.mutation++
      record.pendingCommands--
      this.patch(record, { busy: record.pendingCommands > 0 })
      if (this.current() === record) await this.refresh().catch((error: unknown) => { this.report(record, error) })
    }
  }

  private clearTimer(): void {
    if (this.timer !== undefined) clearTimeout(this.timer)
    this.timer = undefined
  }

  private schedule(): void {
    this.clearTimer()
    const record = this.current()
    if (record === undefined) return
    const delay = active(record.snapshot.status) ? this.options.activePollMs : this.options.pollMs
    this.timer = setTimeout(() => {
      this.timer = undefined
      void this.poll().catch((error: unknown) => { this.report(record, error) })
    }, delay)
  }

  private livePoll(controller: AbortController, generation: number): boolean {
    return !controller.signal.aborted && !this.disposed && generation === this.generation
  }

  private poll(): Promise<void> {
    const record = this.current()
    if (record === undefined) return Promise.resolve()
    if (this.polling?.generation === this.generation) return this.polling.promise
    const generation = this.generation
    const controller = new AbortController()
    const mutation = record.mutation
    const promise = (async () => {
      let response = await this.request({ op: 'execution.poll', workspaceId: record.workspaceId, cursor: record.cursor }, controller.signal)
      if (!this.livePoll(controller, generation)) return
      if (response.cursor < record.cursor) {
        record.cursor = 0
        record.pauseSequences.clear()
        record.pauseKey = undefined
        record.debugEpoch++
        this.patch(record, { ...emptyState(record.workspaceId), truncated: true, busy: record.pendingCommands > 0 })
        response = await this.request({ op: 'execution.poll', workspaceId: record.workspaceId, cursor: 0 }, controller.signal)
      }
      if (!this.livePoll(controller, generation)) return
      this.applyPoll(record, response, record.mutation === mutation && record.pendingCommands === 0)
    })().finally(() => {
      if (this.polling?.controller === controller) this.polling = undefined
      if (!this.disposed && generation === this.generation) this.schedule()
    })
    this.polling = { generation, controller, promise }
    return promise
  }

  private applyPoll(record: RetainedWorkspace, response: IdeExecutionPoll, acceptStatus: boolean): void {
    const previous = record.snapshot
    const outputs = { ...previous.outputs }
    let changedOutput = false
    let truncated = previous.truncated || response.truncated
    const seen = new Set<number>()
    for (const event of response.events) {
      if (event.workspaceId !== record.workspaceId || event.sequence <= record.cursor
        || event.sequence > response.cursor || seen.has(event.sequence)) continue
      seen.add(event.sequence)
      if (event.kind === 'output') {
        outputs[event.operationId] = (outputs[event.operationId] ?? '') + event.text
        changedOutput = true
      } else if (event.kind === 'debug' && event.debug.phase === 'paused') record.pauseSequences.set(event.debug.id, event.sequence)
    }
    record.cursor = Math.max(record.cursor, response.cursor)
    let excess = Object.values(outputs).reduce((total, text) => total + (text?.length ?? 0), 0) - this.options.maxOutputCharacters
    if (excess > 0) {
      for (const id of Object.keys(outputs) as IdeOperationId[]) {
        if (excess <= 0) break
        const text = outputs[id] ?? ''
        const removed = Math.min(excess, text.length)
        outputs[id] = text.slice(removed)
        excess -= removed
      }
      truncated = true
    }
    const status = acceptStatus && JSON.stringify(response.status) !== JSON.stringify(previous.status) ? response.status : previous.status
    const ids = operationIds(status)
    const selected = previous.selected !== null && ids.includes(previous.selected) ? previous.selected : ids.at(-1) ?? null
    const debugId = status.debugSessions.some(debug => debug.id === previous.debugId)
      ? previous.debugId : status.debugSessions.at(-1)?.id ?? null
    this.patch(record, { status, selected, debugId, truncated, error: '', ...changedOutput ? { outputs } : {} })
    if (acceptStatus) this.updateDebug(record)
  }

  private debugSnapshot(record: RetainedWorkspace): IdeDebugSnapshot | undefined {
    return record.snapshot.status.debugSessions.find(debug => debug.id === record.snapshot.debugId)
  }

  private validDebug(record: RetainedWorkspace, debugId: IdeDebugId, epoch: number): boolean {
    return !this.disposed && this.workspaces.get(record.workspaceId) === record && record.debugEpoch === epoch
      && record.snapshot.debugId === debugId && this.debugSnapshot(record)?.phase === 'paused'
  }

  private updateDebug(record: RetainedWorkspace): void {
    const debug = this.debugSnapshot(record)
    if (debug?.phase !== 'paused') {
      if (record.pauseKey !== undefined) {
        record.debugEpoch++
        record.pauseKey = undefined
        this.patch(record, { threads: [], frames: [], scopes: [], variables: {}, watches: {}, threadId: null, frameId: null })
      }
      return
    }
    const key = `${debug.id}:${record.pauseSequences.get(debug.id) ?? 0}:${debug.threadId ?? ''}`
    if (key === record.pauseKey) return
    record.pauseKey = key
    const epoch = ++record.debugEpoch
    void this.loadPause(record, debug, epoch).catch((error: unknown) => {
      if (this.validDebug(record, debug.id, epoch)) this.report(record, error)
    })
  }

  private async loadPause(record: RetainedWorkspace, debug: IdeDebugSnapshot, epoch: number): Promise<void> {
    const threads = await this.request({ op: 'debug.threads', workspaceId: record.workspaceId, debugId: debug.id })
    if (!this.validDebug(record, debug.id, epoch)) return
    const threadId = threads.find(thread => thread.id === debug.threadId)?.id ?? threads.at(0)?.id
    this.patch(record, { threads, threadId: threadId ?? null, frames: [], scopes: [], variables: {}, watches: {}, frameId: null })
    if (threadId !== undefined) await this.loadStack(record, debug.id, threadId, epoch)
  }

  private async loadStack(record: RetainedWorkspace, debugId: IdeDebugId, threadId: number, epoch: number): Promise<void> {
    const frames = await this.request({ op: 'debug.stack', workspaceId: record.workspaceId, debugId, threadId })
    if (!this.validDebug(record, debugId, epoch)) return
    this.patch(record, { frames, threadId })
    const frame = frames.at(0)
    if (frame !== undefined) {
      if (frame.path !== undefined && this.current() === record) {
        try { this.options.onReveal(frame.path, Math.max(1, frame.line), Math.max(1, frame.column)) }
        catch (error) { this.report(record, error) }
      }
      await this.loadFrame(record, debugId, frame.id, epoch)
    }
  }

  private async loadFrame(record: RetainedWorkspace, debugId: IdeDebugId, frameId: number, epoch: number): Promise<void> {
    this.patch(record, { frameId, scopes: [], variables: {}, watches: {} })
    const scopes = await this.request({ op: 'debug.scopes', workspaceId: record.workspaceId, debugId, frameId })
    if (!this.validDebug(record, debugId, epoch)) return
    this.patch(record, { scopes })
    for (const scope of scopes) {
      if (!this.validDebug(record, debugId, epoch)) return
      if (!scope.expensive && scope.variablesReference > 0) await this.loadVariables(record, debugId, scope.variablesReference, epoch)
    }
    if (this.current() !== record || !this.validDebug(record, debugId, epoch)) return
    for (const expression of this.options.getConfiguration().watches) {
      if (!this.validDebug(record, debugId, epoch)) return
      try {
        const result = await this.evaluateInFrame(record, debugId, expression, 'watch')
        if (this.validDebug(record, debugId, epoch)) this.patch(record, { watches: { ...record.snapshot.watches, [expression]: result } })
      } catch (error) {
        if (this.validDebug(record, debugId, epoch)) {
          this.patch(record, { watches: { ...record.snapshot.watches, [expression]: messageOf(error) } })
        }
      }
    }
  }

  private async loadVariables(record: RetainedWorkspace, debugId: IdeDebugId, variablesReference: number, epoch: number): Promise<void> {
    const variables = await this.request({ op: 'debug.variables', workspaceId: record.workspaceId, debugId, variablesReference })
    if (this.validDebug(record, debugId, epoch)) {
      this.patch(record, { variables: { ...record.snapshot.variables, [variablesReference]: variables } })
    }
  }

  private evaluateInFrame(
    record: RetainedWorkspace, debugId: IdeDebugId, expression: string, context: 'watch' | 'repl',
  ): Promise<IdeDebugEvaluation> {
    const frameId = record.snapshot.frameId
    return this.request({ op: 'debug.evaluate', workspaceId: record.workspaceId, debugId, expression, context,
      ...frameId === null ? {} : { frameId } })
  }
}
