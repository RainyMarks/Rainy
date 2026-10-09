// @vitest-environment happy-dom
/** The AI pane shows a chat's turns, sends through the Host (creating the chat first when needed), queues or steers while busy, and runs `/` and `@` input. */
import { act } from 'react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ModelsStatus, SessionSnapshot, SessionSummary, TranscriptEntry, UiPreferences } from '../../src/shared/rpc.ts'
import { emit as emitBus } from '../../src/renderer/app/bus.ts'
import { ChatPane } from '../../src/renderer/chat/ChatPane.tsx'
import { all, button, cleanup, click, emit, fakeHost, flush, handle, hasText, render, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const workspaceId = 'w1' as WorkspaceId
const workspace = { workspaceId, path: '/project', title: 'project' }
const prefs: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true }
const models: ModelsStatus = {
  models: [{ provider: 'fake', model: 'fake-model', baseURL: 'http://127.0.0.1/v1', contextWindow: 32768, local: true }],
  credentials: [], selected: { provider: 'fake', model: 'fake-model' }, thinkingLevels: { fake: ['off'] }, presets: [], globalPrompt: { text: '', maxChars: 4000 },
}

function assistant(content: AssistantMessage['content'], stopReason: AssistantMessage['stopReason'] = 'stop'): AssistantMessage {
  return {
    role: 'assistant', content, api: 'openai-completions', provider: 'fake', model: 'fake-model', stopReason, timestamp: 1,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  }
}

function summary(id: string, change: Partial<SessionSummary> = {}): SessionSummary {
  return { id, workspaceId, cwd: '/project', title: '检查 main.py', createdAt: 1, updatedAt: 2, archived: false, pinned: false, status: 'idle', ...change }
}

function snapshot(id: string, entries: TranscriptEntry[], change: Partial<SessionSnapshot> = {}): SessionSnapshot {
  return { summary: summary(id), entries, model: { provider: 'fake', model: 'fake-model' }, queue: [], context: null, streaming: null, runningTools: [], ...change }
}

const turn: TranscriptEntry[] = [
  { id: 'u1', kind: 'user', ts: 1, text: '看看 @main.py' },
  { id: 'a1', kind: 'assistant', ts: 2, message: assistant([{ type: 'toolCall', id: 'c1', name: 'read', arguments: { file_path: 'main.py' } }], 'toolUse') },
  { id: 'r1', kind: 'toolResult', ts: 3, toolCallId: 'c1', toolName: 'read', content: [{ type: 'text', text: '1: print(1)' }], isError: false,
    details: { path: 'main.py', offset: 1, lines: [{ number: 1, text: 'print(1)' }], totalLines: 1 } },
  { id: 'a2', kind: 'assistant', ts: 4, message: assistant([{ type: 'text', text: '文件只有一行 **print**。' }]) },
  { id: 't1', kind: 'turn', ts: 5, durationMs: 3000, usage: { input: 1200, output: 80, cacheRead: 0, cacheWrite: 0 }, provider: 'fake', model: 'fake-model', requests: 2 },
]

function input(): HTMLTextAreaElement {
  const element = document.querySelector<HTMLTextAreaElement>('[data-chat-input]')
  if (element === null) throw new Error('no composer')
  return element
}

async function key(target: HTMLElement, init: KeyboardEventInit): Promise<void> {
  await act(async () => { target.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init })) })
  await flush()
}

const calls = (method: string): unknown[] => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)

beforeEach(async () => {
  handle('prefs.get', () => prefs)
  handle('models.status', () => models)
  handle('chat.send', () => ({ queued: false }))
  await emit('prefs.changed', prefs)
})
afterEach(async () => { await cleanup() })

it('shows a turn: the user message, the tool card, the answer and the usage footer', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, turn))
  await render(<ChatPane workspace={workspace} sessionId="show-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('Read main.py')).toBe(true) })
  expect(hasText('看看 @main.py')).toBe(true)
  expect(document.body.textContent).toContain('文件只有一行 print。')
  expect(document.body.textContent).toContain('输入 1.2k · 缓存 0 · 输出 80 · 2 次请求')
  expect(hasText('检查 main.py')).toBe(true)
})

it('folds the work steps of finished turns in compact mode', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, turn))
  await emit('prefs.changed', { ...prefs, stepDetail: 'compact' })
  await render(<ChatPane workspace={workspace} sessionId="fold-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(document.body.textContent).toContain('文件只有一行') })
  expect(hasText('Read main.py')).toBe(false)
  const fold = all<HTMLButtonElement>('button').find(item => item.textContent?.startsWith('已处理 3s'))
  expect(fold?.textContent).toContain('读取 1 个文件')
  await click(fold!)
  expect(hasText('Read main.py')).toBe(true)
})

it('creates the chat in the open project before sending the first message', async () => {
  const created = summary('new-1', { title: '' })
  handle('sessions.create', () => created)
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, []))
  const onSessionChange = vi.fn()
  await render(<ChatPane workspace={workspace} sessionId={null} onSessionChange={onSessionChange} onClose={() => undefined} onShowHistory={() => undefined} />)
  expect(hasText('Develop by NCUCyberBase')).toBe(true)
  await type(input(), '修复 bug')
  await key(input(), { key: 'Enter' })
  await waitFor(() => { expect(calls('chat.send')).toEqual([{ sessionId: 'new-1', text: '修复 bug', mode: 'queue' }]) })
  expect(calls('sessions.create')).toEqual([{ workspaceId }])
  expect(onSessionChange).toHaveBeenCalledWith('new-1')
  expect(input().value).toBe('')
})

it('queues with Enter and steers with Ctrl+Enter while busy, and stops on a double Escape', async () => {
  handle('sessions.get', ({ sessionId }) => ({ ...snapshot(sessionId, turn.slice(0, 1)), summary: summary(sessionId, { status: 'running' }) }))
  handle('chat.abort', () => undefined)
  await render(<ChatPane workspace={workspace} sessionId="busy-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(input().placeholder).toBe('回车加入队列，Ctrl+回车 立即插入') })
  await type(input(), '先等等')
  await key(input(), { key: 'Enter' })
  await type(input(), '改成用 Rust')
  await key(input(), { key: 'Enter', ctrlKey: true })
  expect(calls('chat.send')).toEqual([
    { sessionId: 'busy-1', text: '先等等', mode: 'queue' },
    { sessionId: 'busy-1', text: '改成用 Rust', mode: 'steer' },
  ])
  await key(input(), { key: 'Escape' })
  await key(input(), { key: 'Escape' })
  expect(calls('chat.abort')).toEqual([{ sessionId: 'busy-1' }])
  await emit('session.state', { sessionId: 'busy-1', status: 'idle', queue: [], model: null, context: null })
  expect(input().placeholder).toBe('描述任务，@ 引用文件，/ 使用命令')
})

it('runs / commands and completes @ references from the Host', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, turn))
  handle('chat.compact', () => ({ message: '' }))
  handle('chat.complete', () => [{ kind: 'file' as const, insert: 'main.py', label: 'main.py' }])
  await render(<ChatPane workspace={workspace} sessionId="cmd-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('Read main.py')).toBe(true) })
  await type(input(), '/')
  expect(all('[role=option]').map(option => option.textContent)).toEqual(['/compact压缩当前对话中较早的内容', '/model切换当前对话使用的模型', '/new开始新对话'])
  await type(input(), '/compact')
  await key(input(), { key: 'Enter' })
  expect(calls('chat.compact')).toEqual([{ sessionId: 'cmd-1' }])
  expect(input().value).toBe('')

  await type(input(), '看 @ma')
  await waitFor(() => { expect(all('[role=option]').map(option => option.textContent)).toEqual(['main.py']) })
  expect(calls('chat.complete').at(-1)).toEqual({ sessionId: 'cmd-1', workspaceId, query: 'ma' })
  await key(input(), { key: 'Tab' })
  await waitFor(() => { expect(input().value).toBe('看 @main.py ') })
  expect(calls('chat.send')).toEqual([])
})

it('sends selections the editor posts on the bus to the open chat of that project', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, turn))
  await render(<ChatPane workspace={workspace} sessionId="bus-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('Read main.py')).toBe(true) })
  await act(async () => { emitBus('chat.send', { text: '请看这段代码', workspaceId }) })
  await waitFor(() => { expect(calls('chat.send')).toEqual([{ sessionId: 'bus-1', text: '请看这段代码', mode: 'queue' }]) })
})

it('appends streamed entries and shows a failed request', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, turn.slice(0, 1)))
  await render(<ChatPane workspace={workspace} sessionId="live-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('看看 @main.py')).toBe(true) })
  await emit('session.state', { sessionId: 'live-1', status: 'running', queue: [], model: null, context: null })
  await emit('session.stream', { sessionId: 'live-1', message: assistant([{ type: 'text', text: '正在看' }], 'pending') })
  expect(document.body.textContent).toContain('正在看')
  await emit('session.entry', { sessionId: 'live-1', entry: { id: 'a9', kind: 'assistant', ts: 9, message: assistant([], 'error') } })
  await emit('session.state', { sessionId: 'live-1', status: 'error', queue: [], model: null, context: null, error: '429 rate limited' })
  expect(hasText('请求失败')).toBe(true)
  expect(document.body.textContent).toContain('429 rate limited')
  expect(button('发送').disabled).toBe(true)
})

it('says when it is compacting the context', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, turn))
  await render(<ChatPane workspace={workspace} sessionId="compact-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('看看 @main.py')).toBe(true) })
  expect(document.body.textContent).not.toContain('正在压缩上下文')
  await emit('session.state', { sessionId: 'compact-1', status: 'compacting', queue: [], model: null, context: null })
  expect(document.body.textContent).toContain('正在压缩上下文…')
  await emit('session.state', { sessionId: 'compact-1', status: 'idle', queue: [], model: null, context: null })
  expect(document.body.textContent).not.toContain('正在压缩上下文')
})
