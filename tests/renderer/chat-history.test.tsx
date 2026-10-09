// @vitest-environment happy-dom
/** The history list groups chats by project, hides archived ones until asked, searches through the Host, and renames, pins and deletes. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { SessionSummary, UiPreferences } from '../../src/shared/rpc.ts'
import { chatT } from '../../src/renderer/chat/messages.ts'
import { groupChats, History, relativeTime } from '../../src/renderer/chat/History.tsx'
import { all, button, cleanup, click, control, emit, fakeHost, flush, handle, hasText, render, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const current = 'w-current' as WorkspaceId
const other = 'w-other' as WorkspaceId
const prefs: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true }
const now = Date.now()

function chat(id: string, change: Partial<SessionSummary> = {}): SessionSummary {
  return { id, workspaceId: current, cwd: '/work/current', title: id, createdAt: now, updatedAt: now, archived: false, pinned: false, status: 'idle', ...change }
}

const chats = [
  chat('置顶的', { pinned: true, workspaceId: other, cwd: '/work/other' }),
  chat('本项目'),
  chat('别的项目', { workspaceId: other, cwd: '/work/other' }),
  chat('没有项目', { workspaceId: null, cwd: '/home/me' }),
  chat('旧的', { archived: true }),
]

const calls = (method: string): unknown[] => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)

beforeEach(async () => {
  handle('prefs.get', () => prefs)
  handle('sessions.list', () => chats)
  await emit('prefs.changed', prefs)
})
afterEach(async () => { await cleanup() })

it('formats relative times and groups chats by project', () => {
  expect(relativeTime(now - 30_000, now, chatT)).toBe('刚刚')
  expect(relativeTime(now - 5 * 60_000, now, chatT)).toBe('5 分钟前')
  expect(relativeTime(now - 3 * 3_600_000, now, chatT)).toBe('3 小时前')
  expect(relativeTime(now - 2 * 86_400_000, now, chatT)).toBe('2 天前')
  expect(groupChats(chats.slice(0, 4), current).map(group => [group.id, group.chats.map(item => item.id)])).toEqual([
    ['pinned', ['置顶的']], ['current', ['本项目']], ['other', ['别的项目']], ['none', ['没有项目']],
  ])
  expect(groupChats([chats[1]!], null).map(group => group.id)).toEqual(['other'])
})

it('lists unarchived chats in groups and shows archived ones on request', async () => {
  const onSelect = vi.fn()
  await render(<History currentSessionId="本项目" workspaceId={current} onSelect={onSelect} onNewChat={() => undefined} />)
  await waitFor(() => { expect(all('h3').map(heading => heading.textContent)).toEqual(['已置顶', '当前项目', '其他项目', '未关联项目']) })
  expect(hasText('旧的')).toBe(false)
  expect(all('[aria-current=true]').map(item => item.getAttribute('title'))).toEqual(['本项目'])
  await click(document.querySelector<HTMLButtonElement>('button[title="别的项目"]')!)
  expect(onSelect).toHaveBeenCalledWith(chats[2])
  await click(button('显示归档 (1)'))
  expect(hasText('旧的')).toBe(true)
  expect(hasText('本项目')).toBe(false)
  await click(button('隐藏归档'))
  expect(hasText('本项目')).toBe(true)
})

it('renames, pins and deletes through the row menu', async () => {
  handle('sessions.rename', ({ sessionId, title }) => chat(sessionId, { title }))
  handle('sessions.pin', ({ sessionId, pinned }) => chat(sessionId, { pinned }))
  handle('sessions.delete', () => undefined)
  const onNewChat = vi.fn()
  await render(<History currentSessionId="本项目" workspaceId={current} onSelect={() => undefined} onNewChat={onNewChat} />)
  await waitFor(() => { expect(hasText('本项目')).toBe(true) })

  await click(button('本项目 …'))
  await click(all<HTMLButtonElement>('[role=menuitem]').find(item => item.textContent === '重命名')!)
  const field = control<HTMLInputElement>('重命名')
  await type(field, '新名字')
  field.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  await flush()
  expect(calls('sessions.rename')).toEqual([{ sessionId: '本项目', title: '新名字' }])

  await click(button('本项目 …'))
  await click(all<HTMLButtonElement>('[role=menuitem]').find(item => item.textContent === '置顶')!)
  expect(calls('sessions.pin')).toEqual([{ sessionId: '本项目', pinned: true }])

  await click(button('本项目 …'))
  await click(all<HTMLButtonElement>('[role=menuitem]').find(item => item.textContent === '删除')!)
  expect(document.body.textContent).toContain('删除对话“本项目”？此操作无法撤销。')
  await click(all<HTMLButtonElement>('[role=dialog] button').find(item => item.textContent === '删除')!)
  expect(calls('sessions.delete')).toEqual([{ sessionId: '本项目' }])
  expect(onNewChat).toHaveBeenCalled()
})

it('searches chat text through the Host after typing stops', async () => {
  handle('sessions.search', () => [{ summary: chats[2]!, snippet: '…undefined_name 未定义…' }])
  await render(<History currentSessionId={null} workspaceId={current} onSelect={() => undefined} onNewChat={() => undefined} />)
  await type(control<HTMLInputElement>('搜索对话'), 'undefined')
  await waitFor(() => { expect(hasText('…undefined_name 未定义…')).toBe(true) })
  expect(calls('sessions.search')).toEqual([{ query: 'undefined', limit: 30 }])
  expect(all('h3')).toEqual([])
  await type(control<HTMLInputElement>('搜索对话'), '')
  await waitFor(() => { expect(all('h3').length).toBe(4) })
})
