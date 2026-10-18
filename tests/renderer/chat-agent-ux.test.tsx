// @vitest-environment happy-dom
/** Agent feedback in the AI pane: retries after failed requests, thinking time, and a transcript that does not fight the user's scrolling. */
import { act, type MutableRefObject } from 'react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ModelsStatus, SessionSnapshot, SessionSummary, TranscriptEntry, UiPreferences } from '../../src/shared/rpc.ts'
import { ChatPane } from '../../src/renderer/chat/ChatPane.tsx'
import { JUMP_DISTANCE, USER_SCROLL_COOLDOWN_MS, useScrollPin } from '../../src/renderer/chat/scroll-pin.ts'
import { button, cleanup, click, emit, fakeHost, findButton, handle, hasText, render, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const workspaceId = 'w1' as WorkspaceId
const workspace = { workspaceId, path: '/project', title: 'project' }
const prefs: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'compact', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true, webSearch: true }
const models: ModelsStatus = {
  models: [{ provider: 'fake', model: 'fake-model', baseURL: 'http://127.0.0.1/v1', contextWindow: 32768, local: true }],
  credentials: [], selected: { provider: 'fake', model: 'fake-model' }, thinkingLevels: { fake: ['off'] }, presets: [], globalPrompt: { text: '', maxChars: 4000 },
}

function assistant(content: AssistantMessage['content'], stopReason: AssistantMessage['stopReason'] = 'stop', errorMessage?: string): AssistantMessage {
  return {
    role: 'assistant', content, api: 'openai-completions', provider: 'fake', model: 'fake-model', stopReason, timestamp: 1, ...(errorMessage === undefined ? {} : { errorMessage }),
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  }
}

function summary(id: string, change: Partial<SessionSummary> = {}): SessionSummary {
  return { id, workspaceId, cwd: '/project', title: '检查 main.py', createdAt: 1, updatedAt: 2, archived: false, pinned: false, status: 'idle', ...change }
}

function snapshot(id: string, entries: TranscriptEntry[], change: Partial<SessionSnapshot> = {}): SessionSnapshot {
  return { summary: summary(id), entries, model: { provider: 'fake', model: 'fake-model' }, queue: [], context: null, streaming: null, runningTools: [], permission: 'ask', approvals: [], ...change }
}

const usage = { input: 10, output: 1, cacheRead: 0, cacheWrite: 0 }
/** A turn that read a file, then failed on the next request. */
const failedTurn: TranscriptEntry[] = [
  { id: 'u1', kind: 'user', ts: 1, text: '看看 main.py' },
  { id: 'a1', kind: 'assistant', ts: 2, message: assistant([{ type: 'toolCall', id: 'c1', name: 'read', arguments: { file_path: 'main.py' } }], 'toolUse') },
  { id: 'r1', kind: 'toolResult', ts: 3, toolCallId: 'c1', toolName: 'read', content: [{ type: 'text', text: '1: print(1)' }], isError: false },
  { id: 'a2', kind: 'assistant', ts: 4, message: assistant([], 'error', '503 Service Unavailable') },
  { id: 't1', kind: 'turn', ts: 5, durationMs: 3000, usage, provider: 'fake', model: 'fake-model', requests: 2 },
]

const calls = (method: string): unknown[] => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)
const pane = (sessionId: string) => <ChatPane workspace={workspace} sessionId={sessionId} onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />

beforeEach(async () => {
  handle('prefs.get', () => prefs)
  handle('models.status', () => models)
  handle('chat.retry', () => undefined)
  await emit('prefs.changed', prefs)
})
afterEach(async () => { await cleanup() })

it('keeps a failed reply visible in a folded turn and retries it on request', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, failedTurn))
  await render(pane('failed-1'))
  await waitFor(() => { expect(hasText('请求失败')).toBe(true) })
  expect(document.body.textContent).toContain('503 Service Unavailable')
  // The read step stays folded; only the failure is shown.
  expect(hasText('Read main.py')).toBe(false)
  await click(button('重试'))
  expect(calls('chat.retry')).toEqual([{ sessionId: 'failed-1' }])
})

it('offers no retry while the chat runs or once a later message was answered', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, [
    ...failedTurn,
    { id: 'u2', kind: 'user', ts: 6, text: '再试试' },
    { id: 'a3', kind: 'assistant', ts: 7, message: assistant([{ type: 'text', text: '好了' }]) },
  ]))
  await render(pane('answered-1'))
  await waitFor(() => { expect(hasText('好了')).toBe(true) })
  expect(findButton('重试')).toBeUndefined()
})

it('counts down to an automatic retry and retries now on request', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, failedTurn.slice(0, 4), { summary: summary(sessionId, { status: 'running' }) }))
  await render(pane('wait-1'))
  await waitFor(() => { expect(hasText('请求失败')).toBe(true) })
  await emit('session.state', {
    sessionId: 'wait-1', status: 'running', queue: [], model: null, context: null, permission: 'ask', approvals: [],
    retry: { attempt: 2, max: 3, at: Date.now() + 9_500, reason: '503 Service Unavailable' },
  })
  await waitFor(() => { expect(document.body.textContent).toContain('请求失败，10 秒后自动重试（第 2/3 次）') })
  expect(findButton('重试')).toBeUndefined()
  await click(button('立即重试'))
  expect(calls('chat.retry')).toEqual([{ sessionId: 'wait-1' }])
  await emit('session.state', { sessionId: 'wait-1', status: 'running', queue: [], model: null, context: null, permission: 'ask', approvals: [] })
  expect(findButton('立即重试')).toBeUndefined()
})

it('shows how long the model thought, live and afterwards', async () => {
  const thinking: AssistantMessage['content'] = [{ type: 'thinking', thinking: '先看看文件' }, { type: 'text', text: '答案' }]
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, [
    { id: 'u1', kind: 'user', ts: 1, text: '想想' },
    { id: 'a1', kind: 'assistant', ts: 2, message: assistant(thinking), thinkingMs: { 0: 12_400 } },
  ]))
  await render(pane('think-1'))
  await waitFor(() => { expect(hasText('已思考 12s')).toBe(true) })

  await emit('session.state', { sessionId: 'think-1', status: 'running', queue: [], model: null, context: null, permission: 'ask', approvals: [] })
  await emit('session.stream', { sessionId: 'think-1', message: { ...assistant([{ type: 'thinking', thinking: '再想想' }], 'pending' as AssistantMessage['stopReason']), timestamp: 42 } })
  await waitFor(() => { expect(hasText('思考中…')).toBe(true) })
  await waitFor(() => { expect(hasText('思考中… 1s')).toBe(true) }, 3_000)
})

it('keeps a finished reply on screen until its entry replaces it', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, [{ id: 'u1', kind: 'user', ts: 1, text: '说点什么' }], { summary: summary(sessionId, { status: 'running' }) }))
  await render(pane('swap-1'))
  await waitFor(() => { expect(hasText('说点什么')).toBe(true) })
  await emit('session.stream', { sessionId: 'swap-1', message: assistant([{ type: 'text', text: '写到一半的回答' }], 'pending' as AssistantMessage['stopReason']) })
  expect(document.body.textContent).toContain('写到一半的回答')
  await emit('session.stream', { sessionId: 'swap-1', message: null })
  expect(document.body.textContent).toContain('写到一半的回答')
  await emit('session.entry', { sessionId: 'swap-1', entry: { id: 'a1', kind: 'assistant', ts: 2, message: assistant([{ type: 'text', text: '写完的回答' }]) } })
  expect(document.body.textContent).toContain('写完的回答')
  expect(document.body.textContent).not.toContain('写到一半的回答')
})

/** The probe's scrolling box: `height` of content in a 100px viewport, scrolled to `top`. */
let box: { element: HTMLDivElement; height: number; top: number } | undefined

function Probe({ rows }: { rows: number }): JSX.Element {
  const pin = useScrollPin('chat')
  if (box !== undefined) box.height = rows * 100
  return (
    <div data-away={pin.away || undefined} ref={(element) => {
      (pin.ref as MutableRefObject<HTMLDivElement | null>).current = element
      if (element === null || box !== undefined) return
      const state = { element, height: rows * 100, top: 0 }
      Object.defineProperty(element, 'scrollHeight', { get: () => state.height, configurable: true })
      Object.defineProperty(element, 'clientHeight', { get: () => 100, configurable: true })
      Object.defineProperty(element, 'scrollTop', { get: () => state.top, set: (value: number) => { state.top = Math.max(0, Math.min(value, state.height - 100)) }, configurable: true })
      box = state
    }}>
      {Array.from({ length: rows }, (_, index) => <div key={index}>row {index}</div>)}
    </div>
  )
}

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms))
/** The user (or the browser's smooth scrolling) moved the view. */
async function scrollTo(top: number): Promise<void> {
  await act(async () => { box!.top = top; box!.element.dispatchEvent(new Event('scroll')) })
}
async function wheel(deltaY: number): Promise<void> {
  await act(async () => { box!.element.dispatchEvent(new WheelEvent('wheel', { deltaY, bubbles: true })) })
}

it('follows new output, stops at once when the user scrolls up and resumes at the bottom', async () => {
  box = undefined
  const view = await render(<Probe rows={10} />)
  expect(box!.top).toBe(900)

  // A wheel turn upward stops following before the smooth scroll has left the bottom band.
  await wheel(-100)
  await scrollTo(890)
  await view.rerender(<Probe rows={12} />)
  expect(box!.top).toBe(890)
  // 1200 - 890 - 100 = 210 px above the bottom: the jump button shows.
  expect(1200 - 890 - 100).toBeGreaterThan(JUMP_DISTANCE)
  expect(box!.element.dataset.away).toBe('true')

  // Back near the bottom after the cooldown, output is followed again.
  await sleep(USER_SCROLL_COOLDOWN_MS + 20)
  await scrollTo(1080)
  await view.rerender(<Probe rows={14} />)
  expect(box!.top).toBe(1300)
  expect(box!.element.dataset.away).toBeUndefined()
})
