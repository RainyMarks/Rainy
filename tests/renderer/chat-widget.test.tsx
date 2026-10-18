// @vitest-environment happy-dom
/** Interactive answers in the chat: the sandboxed frame, what the page sends it, what it accepts from it, and folding. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ModelsStatus, SessionSnapshot, SessionSummary, TranscriptEntry, UiPreferences } from '../../src/shared/rpc.ts'
import { ChatPane } from '../../src/renderer/chat/ChatPane.tsx'
import { cleanup, emit, fakeHost, handle, hasText, render, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const workspaceId = 'w1' as WorkspaceId
const workspace = { workspaceId, path: '/project', title: 'project' }
const prefs: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true, webSearch: true, widgets: true }
const models: ModelsStatus = {
  models: [], credentials: [], selected: { provider: 'fake', model: 'fake' }, thinkingLevels: {}, presets: [], globalPrompt: { text: '', maxChars: 4000 },
}
const widget = { title: '贷款计算器', height: 320, css: '.out{font-weight:600}', html: '<label>金额 <input id="a" value="100"></label><p class="out" id="o"></p>', script: 'o.textContent = a.value' }

function assistant(content: AssistantMessage['content'], stopReason: AssistantMessage['stopReason'] = 'stop'): AssistantMessage {
  return {
    role: 'assistant', content, api: 'openai-completions', provider: 'fake', model: 'fake', stopReason, timestamp: 1,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  }
}

function snapshot(id: string, entries: TranscriptEntry[], change: Partial<SessionSnapshot> = {}): SessionSnapshot {
  const summary: SessionSummary = { id, workspaceId, cwd: '/project', title: '算贷款', createdAt: 1, updatedAt: 2, archived: false, pinned: false, status: 'idle' }
  return { summary, entries, model: { provider: 'fake', model: 'fake' }, queue: [], context: null, streaming: null, runningTools: [], permission: 'ask', approvals: [], ...change }
}

const turn = (args: Record<string, string | number>, isError = false): TranscriptEntry[] => [
  { id: 'u1', kind: 'user', ts: 1, text: '做个贷款计算器' },
  { id: 'a1', kind: 'assistant', ts: 2, message: assistant([{ type: 'toolCall', id: 'c1', name: 'show_widget', arguments: args }], 'toolUse') },
  { id: 'r1', kind: 'toolResult', ts: 3, toolCallId: 'c1', toolName: 'show_widget', isError, content: [{ type: 'text', text: isError ? 'invalid html: the widget needs markup' : 'The widget is shown to the user.' }] },
  { id: 'a2', kind: 'assistant', ts: 4, message: assistant([{ type: 'text', text: '改金额试试。' }]) },
  { id: 't1', kind: 'turn', ts: 5, durationMs: 1000, usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0, cost: 0 }, provider: 'fake', model: 'fake', requests: 2 },
]

/** The widget's frame with its outgoing messages recorded. */
function frame(): { element: HTMLIFrameElement; sent: Record<string, unknown>[]; say(data: Record<string, unknown>): void } {
  const element = document.querySelector<HTMLIFrameElement>('[data-widget="c1"] iframe')!
  const sent: Record<string, unknown>[] = []
  const target = element.contentWindow!
  target.postMessage = ((data: Record<string, unknown>) => { sent.push(data) }) as typeof target.postMessage
  return {
    element, sent,
    say: (data) => { window.dispatchEvent(new MessageEvent('message', { data: { tag: 'rainy-widget', ...data }, source: target as unknown as Window })) },
  }
}

const open = async (id: string): Promise<void> => {
  await render(<ChatPane workspace={workspace} sessionId={id} onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(document.querySelector('[data-widget="c1"] iframe')).not.toBeNull() })
}

beforeEach(async () => {
  handle('prefs.get', () => prefs)
  handle('models.status', () => models)
  await emit('prefs.changed', prefs)
})
afterEach(async () => { await cleanup(); vi.restoreAllMocks() })

it('shows a widget in a sandboxed frame instead of a tool card and sends it the theme and the finished answer', async () => {
  handle('sessions.get', () => snapshot('w-1', turn(widget)))
  await open('w-1')
  const { element, sent, say } = frame()
  expect(element.getAttribute('sandbox')).toBe('allow-scripts')
  expect(element.getAttribute('src')).toBe('/rainy/widget/frame')
  expect(document.querySelector('[data-tool="show_widget"]')).toBeNull()
  expect(hasText('贷款计算器')).toBe(true)
  expect(hasText('正在生成界面…')).toBe(true)
  say({ type: 'ready' })
  await waitFor(() => { expect(sent.map(message => message.type)).toEqual(['theme', 'final']) })
  expect(sent[0]).toMatchObject({ tag: 'rainy-widget', theme: { dark: false } })
  expect(sent[1]).toEqual({ tag: 'rainy-widget', type: 'final', css: widget.css, html: widget.html, script: widget.script })
  await waitFor(() => { expect(hasText('正在生成界面…')).toBe(false) })
  say({ type: 'resize', height: 512 })
  await waitFor(() => { expect(element.parentElement!.style.height).toBe('512px') })
  // Messages from anywhere else are ignored.
  window.dispatchEvent(new MessageEvent('message', { data: { tag: 'rainy-widget', type: 'resize', height: 90 }, source: window }))
  expect(element.parentElement!.style.height).toBe('512px')
})

it('sends a message for the user only after a click in the widget, and opens only http(s) links', async () => {
  handle('sessions.get', () => snapshot('w-2', turn(widget)))
  handle('chat.send', () => ({ queued: false }))
  const opened = vi.spyOn(window, 'open').mockImplementation(() => null)
  await open('w-2')
  const { element, sent, say } = frame()
  say({ type: 'ready' })
  await waitFor(() => { expect(sent.at(-1)).toMatchObject({ type: 'final' }) })
  say({ type: 'prompt', id: 1, text: '解释一下利率' })
  await waitFor(() => { expect(sent.at(-1)).toMatchObject({ type: 'reply', id: 1, ok: false }) })
  expect(fakeHost.call.mock.calls.some(([method]) => method === 'chat.send')).toBe(false)
  element.focus()
  say({ type: 'prompt', id: 2, text: '解释一下利率' })
  await waitFor(() => { expect(sent.at(-1)).toMatchObject({ type: 'reply', id: 2, ok: true }) })
  expect(fakeHost.call.mock.calls.find(([method]) => method === 'chat.send')?.[1]).toEqual({ sessionId: 'w-2', text: '解释一下利率', mode: 'queue' })
  say({ type: 'link', id: 3, url: 'javascript:alert(1)' })
  await waitFor(() => { expect(sent.at(-1)).toMatchObject({ type: 'reply', id: 3, ok: false }) })
  say({ type: 'link', id: 4, url: 'https://example.com/rates' })
  await waitFor(() => { expect(sent.at(-1)).toMatchObject({ type: 'reply', id: 4, ok: true }) })
  expect(opened).toHaveBeenCalledTimes(1)
  expect(opened).toHaveBeenCalledWith('https://example.com/rates', '_blank', 'noopener,noreferrer')
})

it('reports script errors with a way to ask for a fix', async () => {
  handle('sessions.get', () => snapshot('w-3', turn(widget)))
  handle('chat.send', () => ({ queued: false }))
  await open('w-3')
  const { say } = frame()
  say({ type: 'ready' })
  say({ type: 'error', message: 'o is not defined (line 1)' })
  await waitFor(() => { expect(hasText('界面脚本出错：o is not defined (line 1)')).toBe(true) })
  document.querySelector<HTMLButtonElement>('[data-widget="c1"] [role="alert"] button')!.click()
  await waitFor(() => { expect(fakeHost.call.mock.calls.some(([method]) => method === 'chat.send')).toBe(true) })
  expect(fakeHost.call.mock.calls.find(([method]) => method === 'chat.send')?.[1]).toMatchObject({ text: '交互式回答“贷款计算器”运行时出错：o is not defined (line 1)\n请修复它。' })
})

it('keeps widgets shown when the steps of a finished turn fold', async () => {
  await emit('prefs.changed', { ...prefs, stepDetail: 'compact' })
  handle('sessions.get', () => snapshot('w-4', turn(widget)))
  await open('w-4')
  expect(hasText('改金额试试。')).toBe(true)
  expect(document.querySelector('[aria-expanded="false"]')).not.toBeNull()
})

it('shows a failed call as a tool card', async () => {
  handle('sessions.get', () => snapshot('w-5', turn({ ...widget, html: '' }, true)))
  await render(<ChatPane workspace={workspace} sessionId="w-5" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(document.querySelector('[data-tool="show_widget"]')).not.toBeNull() })
  expect(document.querySelector('[data-widget]')).toBeNull()
})

it('previews a widget while it streams and finishes it once the call is written', async () => {
  const partial = { type: 'toolCall', id: 'c1', name: 'show_widget', arguments: { title: '图表', height: 200, css: 'p{}', html: '<p>第一段</p><p>第二' }, partialArgs: '{...' }
  handle('sessions.get', () => snapshot('w-6', [{ id: 'u1', kind: 'user', ts: 1, text: '画图' }], {
    summary: { id: 'w-6', workspaceId, cwd: '/project', title: '画图', createdAt: 1, updatedAt: 2, archived: false, pinned: false, status: 'running' },
    streaming: assistant([partial as never]),
  }))
  await open('w-6')
  const { sent, say } = frame()
  say({ type: 'ready' })
  await waitFor(() => { expect(sent.map(message => message.type)).toEqual(['theme', 'preview']) })
  expect(sent[1]).toEqual({ tag: 'rainy-widget', type: 'preview', css: 'p{}', html: '<p>第一段</p><p>第二' })
  const { partialArgs: _drop, ...written } = { ...partial, arguments: { ...partial.arguments, html: '<p>第一段</p><p>第二段</p>', script: 'go()' } }
  await emit('session.stream', { sessionId: 'w-6', message: assistant([written as never]) })
  await waitFor(() => { expect(sent.at(-1)).toMatchObject({ type: 'final', html: '<p>第一段</p><p>第二段</p>', script: 'go()' }) })
  expect(document.querySelectorAll('[data-widget="c1"] iframe')).toHaveLength(1)
})

it('enlarges a finished widget into a dialog that runs its own copy', async () => {
  handle('sessions.get', () => snapshot('w-7', turn(widget)))
  await open('w-7')
  document.querySelector<HTMLButtonElement>('[data-widget="c1"] button[aria-label="放大查看"]')!.click()
  await waitFor(() => { expect(document.querySelector('[role="dialog"] [data-widget-enlarged] iframe')).not.toBeNull() })
  const element = document.querySelector<HTMLIFrameElement>('[role="dialog"] iframe')!
  const target = element.contentWindow!
  const sent: Record<string, unknown>[] = []
  target.postMessage = ((data: Record<string, unknown>) => { sent.push(data) }) as typeof target.postMessage
  window.dispatchEvent(new MessageEvent('message', { data: { tag: 'rainy-widget', type: 'ready' }, source: target as unknown as Window }))
  await waitFor(() => { expect(sent.map(message => message.type)).toEqual(['theme', 'final']) })
  expect(document.querySelector('[role="dialog"]')?.getAttribute('aria-label')).toBe('贷款计算器')
})
