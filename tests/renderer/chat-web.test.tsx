// @vitest-environment happy-dom
/** Web search in the chat and in Settings: the search result card, asking before reading a new site, and the switch. */
import { act } from 'react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ModelsStatus, SessionSnapshot, SessionSummary, TranscriptEntry, UiPreferences } from '../../src/shared/rpc.ts'
import { ChatPane } from '../../src/renderer/chat/ChatPane.tsx'
import { WebSearchSettings } from '../../src/renderer/settings/WebSearchSettings.tsx'
import { button, cleanup, click, emit, fakeHost, handle, hasText, render, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const workspaceId = 'w1' as WorkspaceId
const workspace = { workspaceId, path: '/project', title: 'project' }
const prefs: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true, webSearch: true, markdownFocus: false, markdownTypewriter: false, markdownImages: 'assets' }
const deepseek = { provider: 'rainy-deepseek', model: 'deepseek-flash', baseURL: 'https://api.deepseek.com', contextWindow: 1000000, local: false }
const models: ModelsStatus = {
  models: [deepseek], credentials: ['rainy-deepseek'], selected: { provider: 'rainy-deepseek', model: 'deepseek-flash' },
  thinkingLevels: { 'rainy-deepseek': ['off'] }, presets: [], globalPrompt: { text: '', maxChars: 4000 },
}

function assistant(content: AssistantMessage['content'], stopReason: AssistantMessage['stopReason'] = 'stop'): AssistantMessage {
  return {
    role: 'assistant', content, api: 'openai-completions', provider: 'rainy-deepseek', model: 'deepseek-flash', stopReason, timestamp: 1,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  }
}

function summary(id: string, change: Partial<SessionSummary> = {}): SessionSummary {
  return { id, workspaceId, cwd: '/project', title: '查资料', createdAt: 1, updatedAt: 2, archived: false, pinned: false, status: 'idle', ...change }
}

function snapshot(id: string, entries: TranscriptEntry[], change: Partial<SessionSnapshot> = {}): SessionSnapshot {
  return {
    summary: summary(id), entries, model: { provider: 'rainy-deepseek', model: 'deepseek-flash' }, queue: [], context: null, streaming: null,
    runningTools: [], permission: 'ask', approvals: [], background: [], ...change,
  }
}

const calls = (method: string): unknown[] => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)

beforeEach(async () => {
  handle('prefs.get', () => prefs)
  handle('models.status', () => models)
  await emit('prefs.changed', prefs)
})
afterEach(async () => { await cleanup() })

it('shows search results as links with their site, date and snippet', async () => {
  handle('sessions.get', () => snapshot('web-1', [
    { id: 'u1', kind: 'user', ts: 1, text: '查一下 DeepSeek V4.1' },
    { id: 'a1', kind: 'assistant', ts: 2, message: assistant([{ type: 'toolCall', id: 'c1', name: 'web_search', arguments: { query: 'DeepSeek V4.1' } }], 'toolUse') },
    { id: 'r1', kind: 'toolResult', ts: 3, toolCallId: 'c1', toolName: 'web_search', isError: false, content: [{ type: 'text', text: '[1] ...' }],
      details: { query: 'DeepSeek V4.1', results: [{ title: 'DeepSeek V4.1 发布', url: 'https://example.com/v41', site: 'Example', date: '2026-10-17', snippet: '新版本上线' }] } },
  ] as TranscriptEntry[]))
  await render(<ChatPane workspace={workspace} sessionId="web-1" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('网页搜索 DeepSeek V4.1')).toBe(true) })
  expect(hasText('1 条结果')).toBe(true)
  await click(document.querySelector<HTMLButtonElement>('[data-tool="web_search"] button[aria-expanded]')!)
  const link = document.querySelector<HTMLAnchorElement>('[data-tool="web_search"] a')!
  expect(link.textContent).toBe('DeepSeek V4.1 发布')
  expect(link.getAttribute('href')).toBe('https://example.com/v41')
  expect(link.getAttribute('target')).toBe('_blank')
  expect(hasText('Example · 2026-10-17')).toBe(true)
  expect(hasText('新版本上线')).toBe(true)
})

it('asks before reading a page from a new site and can remember the site', async () => {
  handle('sessions.get', () => snapshot('web-2', [
    { id: 'u1', kind: 'user', ts: 1, text: '读一下这个页面' },
    { id: 'a1', kind: 'assistant', ts: 2, message: assistant([{ type: 'toolCall', id: 'c1', name: 'web_fetch', arguments: { url: 'https://docs.example.com/a' } }], 'toolUse') },
  ], {
    summary: summary('web-2', { status: 'running', awaitingApproval: true }), runningTools: ['c1'],
    approvals: [{ toolCallId: 'c1', toolName: 'web_fetch', reason: 'web', path: 'https://docs.example.com/a', rule: 'web_fetch:docs.example.com' }],
  }))
  handle('chat.approve', () => undefined)
  await render(<ChatPane workspace={workspace} sessionId="web-2" onSessionChange={() => undefined} onClose={() => undefined} onShowHistory={() => undefined} />)
  await waitFor(() => { expect(hasText('读取这个网页需要你的批准：https://docs.example.com/a')).toBe(true) })
  await click(button('总是允许 docs.example.com'))
  expect(calls('chat.approve')).toEqual([{ sessionId: 'web-2', toolCallId: 'c1', decision: 'always' }])
})

it('turns web search off and says when a DeepSeek key is missing', async () => {
  handle('prefs.set', change => ({ ...prefs, ...change }))
  const view = await render(<WebSearchSettings status={models} />)
  expect(hasText('已就绪：使用你的 DeepSeek 模型的 API 密钥。')).toBe(true)
  await view.rerender(<WebSearchSettings status={{ ...models, credentials: [] }} />)
  expect(hasText('需要 DeepSeek API 密钥：在上方添加 DeepSeek V4.1 Flash 并填写密钥后即可使用。')).toBe(true)
  const toggle = document.querySelector<HTMLButtonElement>('[role="switch"][aria-label="允许 AI 联网搜索"]')!
  expect(toggle.getAttribute('aria-checked')).toBe('true')
  await click(toggle)
  expect(calls('prefs.set')).toEqual([{ webSearch: false }])
  await act(async () => { await emit('prefs.changed', { ...prefs, webSearch: false }) })
  await waitFor(() => { expect(toggle.getAttribute('aria-checked')).toBe('false') })
})
