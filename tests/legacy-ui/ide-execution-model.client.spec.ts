/** Workspace switching, bounded output, debugger races and client-request teardown. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { WorkspaceId } from '../src/ide-files-protocol.ts'
import type {
  IdeDebugId, IdeExecutionConfiguration, IdeExecutionPoll, IdeExecutionRequest, IdeExecutionResponse,
  IdeExecutionResponseMap, IdeExecutionStatus, IdeRunId, IdeRunSnapshot, IdeTerminalId,
} from '../src/ide-execution-protocol.ts'
import type { IdeExecutionApi } from '../src/client/ide-execution-api.ts'
import { IdeExecutionModel } from '../src/client/ide-execution-model.ts'
import type { IdeExecutionState } from '../src/client/ide-execution-model.ts'

const owners: IdeExecutionModel[] = []
const releases: Array<() => void> = []
afterEach(async () => {
  for (const release of releases.splice(0)) release()
  await Promise.all(owners.splice(0).map(owner => owner.dispose()))
  vi.useRealTimers()
  vi.restoreAllMocks()
})
const workspaceA = brandString<WorkspaceId>('workspace-a')
const workspaceB = brandString<WorkspaceId>('workspace-b')
const terminalA = brandString<IdeTerminalId>('terminal-a')
const terminalB = brandString<IdeTerminalId>('terminal-b')
const debugId = brandString<IdeDebugId>('debug-a')
const emptyStatus = (): IdeExecutionStatus => ({ runs: [], terminals: [], debugSessions: [] })
const terminalStatus = (workspaceId: WorkspaceId, id: IdeTerminalId): IdeExecutionStatus => ({ ...emptyStatus(),
  terminals: [{ id, workspaceId, cwd: '/workspace', phase: 'running' }] })
const poll = (status: IdeExecutionStatus = emptyStatus(), cursor = 0): IdeExecutionPoll =>
  ({ status, cursor, truncated: false, events: [] })

function fixture(
  handler: (request: IdeExecutionRequest, signal?: AbortSignal) => Promise<IdeExecutionResponse>,
  changes: Partial<{ pollMs: number; activePollMs: number; maxOutputCharacters: number }> = {},
) {
  const calls: IdeExecutionRequest[] = []
  const api: IdeExecutionApi = {
    async request<K extends IdeExecutionRequest['op']>(request: Extract<IdeExecutionRequest, { op: K }>, signal?: AbortSignal) {
      calls.push(request)
      // The fixture handler returns protocol values; the method-key correlation remains generic.
      return await handler(request, signal) as IdeExecutionResponseMap[K]
    },
  }
  let configuration: IdeExecutionConfiguration = { profiles: [], activeProfile: null, breakpoints: [], watches: [] }
  const onError = vi.fn()
  const onReveal = vi.fn()
  const owner = new IdeExecutionModel(api, { pollMs: 100000, activePollMs: 100000, maxOutputCharacters: 1024,
    maxRetainedWorkspaces: 4, terminalCols: 80, terminalRows: 24, ...changes,
    getConfiguration: () => configuration, setConfiguration: (value) => { configuration = value }, onError, onReveal })
  owners.push(owner)
  return { owner, calls, onError, onReveal, configuration: () => configuration,
    configure: (value: IdeExecutionConfiguration) => { configuration = value } }
}

async function stateWhen(owner: IdeExecutionModel, predicate: (state: IdeExecutionState) => boolean): Promise<void> {
  if (predicate(owner.state.getSnapshot())) return
  const arrived = Promise.withResolvers<undefined>()
  const unsubscribe = owner.state.subscribe(() => { if (predicate(owner.state.getSnapshot())) arrived.resolve(undefined) })
  try { if (!predicate(owner.state.getSnapshot())) await arrived.promise }
  finally { unsubscribe() }
}

describe('workspace execution observations', () => {
  it('ignores a late workspace poll and restores the previous workspace without stopping its terminal', async () => {
    const late = Promise.withResolvers<IdeExecutionResponse>()
    const responseA: IdeExecutionPoll = { ...poll(terminalStatus(workspaceA, terminalA), 2),
      events: [{ sequence: 1, workspaceId: workspaceA, kind: 'output', operationId: terminalA, stream: 'terminal', text: 'A' }] }
    const responseB: IdeExecutionPoll = { ...poll(terminalStatus(workspaceB, terminalB), 2),
      events: [{ sequence: 2, workspaceId: workspaceB, kind: 'output', operationId: terminalB, stream: 'terminal', text: 'B' }] }
    let firstA = true
    releases.push(() => { late.resolve(responseA) })
    const { owner, calls } = fixture(async (request) => {
      if (request.workspaceId === workspaceA && firstA) { firstA = false; return late.promise }
      return request.workspaceId === workspaceA ? responseA : responseB
    })
    owner.setWorkspace(workspaceA)
    owner.setWorkspace(workspaceB)
    await stateWhen(owner, state => state.outputs[terminalB] === 'B')
    late.resolve(responseA)
    await owner.refresh()
    expect(owner.state.getSnapshot()).toMatchObject({ workspaceId: workspaceB, outputs: { [terminalB]: 'B' } })
    expect(owner.state.getSnapshot().outputs[terminalA]).toBeUndefined()
    owner.setWorkspace(workspaceA)
    await stateWhen(owner, state => state.outputs[terminalA] === 'A')
    expect(calls.some(call => call.op.endsWith('.stop'))).toBe(false)
  })

  it('deduplicates event sequences and trims retained output to its workspace budget', async () => {
    const event = { sequence: 2, workspaceId: workspaceA, kind: 'output' as const, operationId: terminalA,
      stream: 'terminal' as const, text: 'ghij' }
    const response: IdeExecutionPoll = { ...poll(terminalStatus(workspaceA, terminalA), 2), events: [
      { ...event, sequence: 1, text: 'abcdef' }, event, event,
    ] }
    const { owner } = fixture(async () => response, { maxOutputCharacters: 6 })
    owner.setWorkspace(workspaceA)
    await stateWhen(owner, state => state.outputs[terminalA] === 'efghij')
    expect(owner.state.getSnapshot().truncated).toBe(true)
    await owner.refresh()
    expect(owner.state.getSnapshot().outputs[terminalA]).toBe('efghij')
  })

  it('replaces output from a restarted Host when its event cursor moves backwards', async () => {
    let response: IdeExecutionPoll = { ...poll(terminalStatus(workspaceA, terminalA), 8), events: [
      { sequence: 8, workspaceId: workspaceA, kind: 'output', operationId: terminalA, stream: 'terminal', text: 'old' },
    ] }
    const { owner, calls } = fixture(async () => response)
    owner.setWorkspace(workspaceA)
    await stateWhen(owner, state => state.outputs[terminalA] === 'old')
    response = { ...poll(terminalStatus(workspaceA, terminalB), 1), events: [
      { sequence: 1, workspaceId: workspaceA, kind: 'output', operationId: terminalB, stream: 'terminal', text: 'new' },
    ] }
    await owner.refresh()
    expect(owner.state.getSnapshot().outputs).toEqual({ [terminalB]: 'new' })
    expect(calls.at(-1)).toMatchObject({ op: 'execution.poll', cursor: 0 })
  })

  it('uses the active cadence only while processes remain alive and clears its timer on disposal', async () => {
    vi.useFakeTimers()
    let status = terminalStatus(workspaceA, terminalA)
    const { owner, calls } = fixture(async () => poll(status), { activePollMs: 50, pollMs: 500 })
    owner.setWorkspace(workspaceA)
    await owner.refresh()
    let count = calls.length
    await vi.advanceTimersByTimeAsync(49)
    expect(calls).toHaveLength(count)
    await vi.advanceTimersByTimeAsync(1)
    expect(calls).toHaveLength(++count)
    status = emptyStatus()
    await vi.advanceTimersByTimeAsync(50)
    expect(calls).toHaveLength(++count)
    await vi.advanceTimersByTimeAsync(499)
    expect(calls).toHaveLength(count)
    await vi.advanceTimersByTimeAsync(1)
    expect(calls).toHaveLength(count + 1)
    await owner.dispose()
    expect(vi.getTimerCount()).toBe(0)
  })
})

describe('human debug and process commands', () => {
  it('loads paused frames and variables while suppressing a late response for a previously selected frame', async () => {
    const staleScopes = Promise.withResolvers<IdeExecutionResponse>()
    releases.push(() => { staleScopes.resolve([{ name: 'stale', variablesReference: 99, expensive: false }]) })
    const status: IdeExecutionStatus = { ...emptyStatus(), debugSessions: [{ id: debugId, workspaceId: workspaceA,
      name: 'Python', language: 'python', phase: 'paused', threadId: 1, breakpoints: [] }] }
    const { owner, onReveal } = fixture(async (request) => {
      switch (request.op) {
        case 'execution.poll': return poll(status)
        case 'debug.threads': return [{ id: 1, name: 'main' }]
        case 'debug.stack': return [{ id: 1, name: 'first', path: 'main.py', line: 3, column: 1 },
          { id: 2, name: 'second', path: 'caller.py', line: 7, column: 1 }]
        case 'debug.scopes': return request.frameId === 1 ? staleScopes.promise : [{ name: 'locals', variablesReference: 20, expensive: false }]
        case 'debug.variables': return [{ name: 'counter', value: '2', variablesReference: 0 }]
        default: return { ok: true }
      }
    })
    owner.setWorkspace(workspaceA)
    await stateWhen(owner, state => state.frames.length === 2 && state.frameId === 1)
    await owner.selectFrame(2)
    staleScopes.resolve([{ name: 'stale', variablesReference: 99, expensive: false }])
    await owner.refresh()
    expect(owner.state.getSnapshot()).toMatchObject({ frameId: 2, scopes: [{ name: 'locals', variablesReference: 20 }],
      variables: { 20: [{ name: 'counter', value: '2' }] } })
    expect(owner.state.getSnapshot().variables[99]).toBeUndefined()
    expect(onReveal).toHaveBeenCalledWith('main.py', 3, 1)
  })

  it('keeps saved profiles and watch expressions while toggling and applying source breakpoints', async () => {
    const { owner, configure, configuration, calls } = fixture(async request => request.op === 'execution.poll' ? poll() : { ok: true })
    configure({ profiles: [{ name: 'module', language: 'python', program: 'main.py', pythonModule: 'demo.main', executable: '/env/bin/python' }],
      activeProfile: 'module', breakpoints: [], watches: ['counter'] })
    owner.setWorkspace(workspaceA)
    await owner.refresh()
    await owner.toggleBreakpoint('main.py', 3)
    expect(configuration()).toMatchObject({ activeProfile: 'module', watches: ['counter'],
      profiles: [{ pythonModule: 'demo.main', executable: '/env/bin/python' }], breakpoints: [{ path: 'main.py', lines: [3] }] })
    await owner.toggleBreakpoint('main.py', 3)
    expect(configuration().breakpoints).toEqual([])
    expect(calls.some(request => request.op === 'debug.start')).toBe(false)
  })

  it('does not let an older in-flight poll discard an admitted run', async () => {
    const heldPoll = Promise.withResolvers<IdeExecutionResponse>()
    const pollEntered = Promise.withResolvers<undefined>()
    const runEntered = Promise.withResolvers<undefined>()
    const releaseRun = Promise.withResolvers<undefined>()
    const oldStatus = emptyStatus()
    const id = brandString<IdeRunId>('run-a')
    const run: IdeRunSnapshot = { id, workspaceId: workspaceA, name: 'main', phase: 'running', spec: {
      workspaceId: workspaceA, workspaceRoot: '/workspace', name: 'main', program: '/workspace/main.py', language: 'python',
      launch: { argv: ['python3', 'main.py'], cwd: '/workspace', environment: {} }, build: [], terminal: true,
    } }
    releases.push(() => { heldPoll.resolve(poll(oldStatus)); releaseRun.resolve(undefined) })
    let holdNextPoll = false
    let latest = oldStatus
    const { owner } = fixture(async (request) => {
      if (request.op === 'execution.poll') {
        if (holdNextPoll) { holdNextPoll = false; pollEntered.resolve(undefined); return heldPoll.promise }
        return poll(latest)
      }
      if (request.op === 'run.start') { runEntered.resolve(undefined); await releaseRun.promise; latest = { ...emptyStatus(), runs: [run] }; return run }
      return { ok: true }
    })
    owner.setWorkspace(workspaceA)
    await owner.refresh()
    const starting = owner.run({ name: 'main', language: 'python', program: 'main.py' })
    await runEntered.promise
    holdNextPoll = true
    const earlier = owner.refresh()
    await pollEntered.promise
    releaseRun.resolve(undefined)
    await stateWhen(owner, state => state.status.runs.length === 1)
    heldPoll.resolve(poll(oldStatus))
    await Promise.all([earlier, starting])
    expect(owner.state.getSnapshot().status.runs).toEqual([run])
  })

  it('aborts and joins outstanding reads on disposal without publishing a late error', async () => {
    const entered = Promise.withResolvers<undefined>()
    const aborted = Promise.withResolvers<undefined>()
    const { owner, onError } = fixture((_request, signal) => new Promise((_accept, reject) => {
      entered.resolve(undefined)
      signal?.addEventListener('abort', () => {
        aborted.resolve(undefined)
        reject(new DOMException('The fixture request was aborted.', 'AbortError'))
      }, { once: true })
    }))
    owner.setWorkspace(workspaceA)
    await entered.promise
    const snapshot = owner.state.getSnapshot()
    await owner.dispose()
    await aborted.promise
    expect(owner.state.getSnapshot()).toBe(snapshot)
    expect(onError).not.toHaveBeenCalled()
  })
})
