// @vitest-environment happy-dom
/** Approval bars on waiting tool calls, the composer's permission mode, undo of file changes, and the Security settings section. */
import { act } from 'react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ModelsStatus, SecurityStatus, SessionSnapshot, SessionSummary, TranscriptEntry, UiPreferences } from '../../src/shared/rpc.ts'
import { ChatPane } from '../../src/renderer/chat/ChatPane.tsx'
import { SecuritySection } from '../../src/renderer/settings/SecuritySection.tsx'
import { all, button, choose, cleanup, click, emit, fakeHost, flush, handle, hasText, render, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const workspaceId = 'w1' as WorkspaceId
const workspace = { workspaceId, path: '/project', title: 'project' }
const prefs: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true, webSearch: true }
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
  return { id, workspaceId, cwd: '/project', title: '解题', createdAt: 1, updatedAt: 2, archived: false, pinned: false, status: 'idle', ...change }
}

function snapshot(id: string, entries: TranscriptEntry[], change: Partial<SessionSnapshot> = {}): SessionSnapshot {
  return {
    summary: summary(id), entries, model: { provider: 'fake', model: 'fake-model' }, queue: [], context: null, streaming: null, runningTools: [],
    permission: 'ask', approvals: [], background: [], ...change,
  }
}

const calls = (method: string): unknown[] => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)

beforeEach(async () => {
  handle('prefs.get', () => prefs)
  handle('models.status', () => models)
  await emit('prefs.changed', prefs)
})
afterEach(async () => { await cleanup() })

const waiting: TranscriptEntry[] = [
  { id: 'u1', kind: 'user', ts: 1, text: '跑一下脚本' },
  { id: 'a1', kind: 'assistant', ts: 2, message: assistant([{ type: 'toolCall', id: 'c1', name: 'bash', arguments: { command: 'python3 solve.py', description: 'Run the solver' } }], 'toolUse') },
]

async function showWaiting(sessionId: string): Promise<void> {
  handle('sessions.get', () => snapshot(sessionId, waiting, {
    summary: summary(sessionId, { status: 'running', awaitingApproval: true }), runningTools: ['c1'],
    approvals: [{ toolCallId: 'c1', toolName: 'bash', reason: 'command', rule: 'python3' }],
  }))
  handle('chat.approve', () => undefined)
  await render(<ChatPane workspace={workspace} sessionId={sessionId} onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('运行这条命令需要你的批准')).toBe(true) })
}

it('asks before a command runs and sends the answer to the Host', async () => {
  await showWaiting('ask-1')
  expect(hasText('等待批准')).toBe(true)
  expect(document.body.textContent).toContain('python3 solve.py')
  // The command has not started while it waits.
  expect(hasText('运行中')).toBe(false)
  await click(button('总是允许 python3'))
  expect(calls('chat.approve')).toEqual([{ sessionId: 'ask-1', toolCallId: 'c1', decision: 'always' }])
})

it('shows the next chat the default mode Settings just chose', async () => {
  handle('security.status', () => ({ defaultMode: 'ask' as const, rules: { commands: [], tools: [] } }))
  await render(<ChatPane workspace={workspace} sessionId={null} onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(all('[data-permission="ask"]')).toHaveLength(1) })
  await emit('security.changed', { defaultMode: 'auto' })
  await waitFor(() => { expect(all('[data-permission="auto"]')).toHaveLength(1) })
})

it('denies with Esc, or with an instruction typed into the bar', async () => {
  await showWaiting('ask-2')
  const bar = document.querySelector<HTMLElement>('[data-approval="command"]')!
  await act(async () => { bar.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true })) })
  await flush()
  expect(calls('chat.approve')).toEqual([{ sessionId: 'ask-2', toolCallId: 'c1', decision: 'deny' }])
  await cleanup()
  await showWaiting('ask-3')
  const reason = document.querySelector<HTMLInputElement>('[data-approval="command"] input')!
  await type(reason, '用 python 而不是 bash')
  await act(async () => { reason.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true, cancelable: true })) })
  await flush()
  expect(calls('chat.approve').at(-1)).toEqual({ sessionId: 'ask-3', toolCallId: 'c1', decision: 'deny', reason: '用 python 而不是 bash' })
})

it('switches the chat permission mode from the composer', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, []))
  handle('chat.setPermission', ({ mode }) => mode)
  await render(<ChatPane workspace={workspace} sessionId="mode-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(all('[data-permission="ask"]')).toHaveLength(1) })
  await click(all<HTMLButtonElement>('[data-permission="ask"]')[0]!)
  const item = all<HTMLElement>('[role="menuitem"]').find(element => element.textContent?.startsWith('完全自动'))
  await click(item!)
  expect(calls('chat.setPermission')).toEqual([{ sessionId: 'mode-1', mode: 'auto' }])
})

const changed: TranscriptEntry[] = [
  { id: 'u1', kind: 'user', ts: 1, text: '改一下' },
  { id: 'a1', kind: 'assistant', ts: 2, message: assistant([
    { type: 'toolCall', id: 'w1', name: 'write', arguments: { file_path: 'a.py', content: 'print(2)\n' } },
    { type: 'toolCall', id: 'w2', name: 'edit', arguments: { file_path: 'b.py', old_string: 'x', new_string: 'y' } },
  ], 'toolUse') },
  { id: 'r1', kind: 'toolResult', ts: 3, toolCallId: 'w1', toolName: 'write', content: [{ type: 'text', text: 'ok' }], isError: false,
    details: { path: 'a.py', operation: 'update', before: 'print(1)\n', after: 'print(2)\n', undoable: true } },
  { id: 'r2', kind: 'toolResult', ts: 4, toolCallId: 'w2', toolName: 'edit', content: [{ type: 'text', text: 'ok' }], isError: false,
    details: { path: 'b.py', operation: 'update', before: 'x\n', after: 'y\n', undoable: true } },
  { id: 'a2', kind: 'assistant', ts: 5, message: assistant([{ type: 'text', text: '改好了。' }]) },
  { id: 't1', kind: 'turn', ts: 6, durationMs: 1000, usage: { input: 10, output: 5, cacheRead: 0, cacheWrite: 0 }, provider: 'fake', model: 'fake-model', requests: 2 },
]

it('undoes one change from its card, and a whole turn after confirming', async () => {
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, changed))
  handle('chat.revert', ({ toolCallIds }) => ({ reverted: toolCallIds.map(toolCallId => ({ toolCallId, path: '/project/a.py' })), failed: [] }))
  await render(<ChatPane workspace={workspace} sessionId="undo-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('写入 a.py')).toBe(true) })
  await click(all<HTMLButtonElement>('button').find(element => element.textContent?.includes('写入 a.py'))!)
  await click(button('撤销此修改'))
  expect(calls('chat.revert')).toEqual([{ sessionId: 'undo-1', toolCallIds: ['w1'] }])
  await click(button('撤销本轮修改'))
  await waitFor(() => { expect(hasText('撤销本轮的文件修改？')).toBe(true) })
  expect(document.body.textContent).toContain('2 处修改会恢复到本轮之前的内容')
  await click(all<HTMLButtonElement>('[role="dialog"] button').find(element => element.textContent === '撤销本轮修改')!)
  expect(calls('chat.revert').at(-1)).toEqual({ sessionId: 'undo-1', toolCallIds: ['w1', 'w2'] })
})

it('marks undone changes and offers no second undo', async () => {
  const entries: TranscriptEntry[] = [...changed, { id: 'c1', kind: 'context', ts: 7, label: 'notice', text: 'The user undid 2 of your file changes.', reverted: ['w1', 'w2'] }]
  handle('sessions.get', ({ sessionId }) => snapshot(sessionId, entries))
  await render(<ChatPane workspace={workspace} sessionId="undo-2" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('已撤销 2 处文件修改')).toBe(true) })
  expect(all('button').some(element => element.textContent === '撤销本轮修改')).toBe(false)
  expect(all('span').filter(element => element.textContent === '已撤销')).toHaveLength(2)
})

it('sets the default mode and removes remembered rules in Settings → Security', async () => {
  let status: SecurityStatus = { defaultMode: 'ask', rules: { commands: ['python3', 'git status'], tools: ['mcp__ida__rename'] } }
  handle('security.status', () => status)
  handle('security.setDefault', ({ mode }) => { status = { ...status, defaultMode: mode }; return status })
  handle('security.removeRule', ({ rule }) => {
    status = { ...status, rules: { commands: status.rules.commands.filter(item => item !== rule), tools: status.rules.tools.filter(item => item !== rule) } }
    return status
  })
  await render(<SecuritySection workspace={workspace} />)
  await waitFor(() => { expect(hasText('python3')).toBe(true) })
  expect(hasText('ida: rename')).toBe(true)
  await choose('新对话的权限', '只读：只能读取和搜索')
  expect(calls('security.setDefault')).toEqual([{ mode: 'read-only' }])
  const row = all<HTMLElement>('li').find(element => element.textContent?.includes('git status'))!
  await click(button('移除', row))
  expect(calls('security.removeRule')).toEqual([{ workspaceId, kind: 'command', rule: 'git status' }])
  await waitFor(() => { expect(hasText('git status')).toBe(false) })
})

it('turns off keeping the agent changes automatically in Settings → Security', async () => {
  handle('security.status', () => ({ defaultMode: 'ask' as const, rules: { commands: [], tools: [] } }))
  handle('prefs.set', change => ({ ...prefs, ...change }))
  await render(<SecuritySection workspace={workspace} />)
  const toggle = document.querySelector<HTMLButtonElement>('[role="switch"][aria-label="自动保留 AI 的修改"]')!
  expect(toggle.getAttribute('aria-checked')).toBe('true')
  await click(toggle)
  expect(calls('prefs.set')).toEqual([{ autoKeepChanges: false }])
  await waitFor(() => { expect(toggle.getAttribute('aria-checked')).toBe('false') })
})
