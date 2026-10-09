// @vitest-environment happy-dom
/** The window keeps the chat, the project and the editor in step: history, bus events, shortcuts and the project menu. */
import { act } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { IdeWorkspaceState } from '../../src/shared/ide-files-protocol.ts'
import type { SessionSummary } from '../../src/shared/rpc.ts'
import { emit, on, type WorkbenchEvents } from '../../src/renderer/app/bus.ts'
import { byRole, change, cleanup, click, keyDown, queryByRole, waitFor } from './ide-dom.tsx'
import { chatState, resetChatState } from './app-chat-mock.tsx'
import { disposeWindows, document as source, mountWindow, other, workspace } from './app-fixture.tsx'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
vi.mock('../../src/renderer/chat/ChatPane.tsx', () => import('./app-chat-mock.tsx'))
vi.mock('../../src/renderer/chat/History.tsx', () => import('./app-chat-mock.tsx'))
vi.mock('../../src/renderer/settings/SettingsDialog.tsx', () => import('./app-chat-mock.tsx'))
vi.mock('../../src/renderer/ide/editor-loader.ts', () => ({ loadEditorAssets: async () => (await import('./app-fixture.tsx')).assets }))

afterEach(async () => {
  cleanup()
  await disposeWindows()
  resetChatState()
  vi.clearAllMocks()
})

const savedState = (lastSessionId: string | null = null): IdeWorkspaceState => ({ version: 1, revision: 0, data: {
  lastSessionId, tabs: [], activePath: null, expandedPaths: [], buffers: [],
  layout: { sidebarWidth: 240, agentWidth: 400, bottomHeight: 240, sidebarVisible: true, agentVisible: false, bottomVisible: false, bottomTab: 'terminal' },
} })
const summary = (id: string, owner = workspace): SessionSummary => ({ id, workspaceId: owner.workspaceId, cwd: owner.path, title: id,
  createdAt: 0, updatedAt: 0, archived: false, pinned: false, status: 'idle' })
const press = (code: string, modifiers: { alt?: boolean } = {}): void => {
  keyDown(window, { code, key: code, ctrlKey: true, altKey: modifiers.alt ?? false })
}

describe('chat and project', () => {
  it('opens the project of a chat picked from the history and shows that chat', async () => {
    const h = await mountWindow()
    h.request.mockResolvedValueOnce(savedState('older-chat')).mockResolvedValueOnce({ path: '', entries: [] })
      .mockResolvedValueOnce({ version: 1, workspaceId: other.workspaceId })
    await act(async () => { await chatState.history?.onSelect(summary('chat-b', other)) })
    const state = h.workbench.model.state.getSnapshot()
    expect(state.workspace).toEqual(other)
    expect(state.data.lastSessionId).toBe('chat-b')
    expect(state.data.layout.agentVisible).toBe(true)
    expect(chatState.chat?.sessionId).toBe('chat-b')
    expect(chatState.chat?.workspace).toEqual(other)
  })

  it('shows a chat of the current project from the history and remembers it for the project', async () => {
    const h = await mountWindow()
    await act(async () => { await chatState.history?.onSelect(summary('chat-a')) })
    expect(h.request).not.toHaveBeenCalled()
    expect(chatState.chat?.sessionId).toBe('chat-a')
    expect(h.workbench.model.state.getSnapshot().data).toMatchObject({ lastSessionId: 'chat-a', layout: { agentVisible: true } })
    act(() => { chatState.chat?.onSessionChange('created-chat') })
    expect(h.workbench.model.state.getSnapshot().data.lastSessionId).toBe('created-chat')
    act(() => { chatState.history?.onNewChat() })
    expect(chatState.chat?.sessionId).toBeNull()
    expect(h.workbench.model.state.getSnapshot().data.lastSessionId).toBeNull()
  })

  it('restores a remembered chat only when it belongs to the opened project', async () => {
    const h = await mountWindow(1440, { sessionInWorkspace: sessionId => sessionId === 'kept' })
    h.request.mockResolvedValueOnce(savedState('foreign')).mockResolvedValueOnce({ path: '', entries: [] })
      .mockResolvedValueOnce({ version: 1, workspaceId: other.workspaceId })
    await act(async () => { await h.workbench.model.selectWorkspace(other) })
    expect(chatState.chat?.sessionId).toBeNull()
    h.request.mockResolvedValueOnce(savedState('kept')).mockResolvedValueOnce({ path: '', entries: [] })
      .mockResolvedValueOnce({ version: 1, workspaceId: workspace.workspaceId })
    await act(async () => { await h.workbench.model.selectWorkspace(workspace) })
    expect(chatState.chat?.sessionId).toBe('kept')
  })

  it('sends the editor selection to the AI composer and shows the AI pane', async () => {
    const h = await mountWindow()
    const sent: WorkbenchEvents['chat.send'][] = []
    const stop = on('chat.send', (event) => { sent.push(event) })
    act(() => {
      h.workbench.model.selection({ path: 'main.py', text: 'x = 1', language: 'python', startLine: 1, startColumn: 1, endLine: 1, endColumn: 6 })
    })
    click(byRole('button', '编辑器操作'))
    click(byRole('menuitem', '将选中代码发送到 AI'))
    stop()
    expect(sent).toEqual([{ workspaceId: workspace.workspaceId,
      text: '请分析以下选中的代码：\n文件：/tmp/project/main.py\n范围：1:1–1:6\n\n```python\nx = 1\n```' }])
    expect(byRole('complementary', 'AI 助手')).toBeTruthy()
  })
})

describe('workbench events', () => {
  it('opens a chat file link at its line and reports paths outside the project', async () => {
    const h = await mountWindow()
    h.request.mockResolvedValueOnce({ ...source, path: 'lib/tool.py' })
    act(() => { emit('editor.open', { path: '/tmp/project/lib/tool.py', line: 3 }) })
    await waitFor(() => { expect(h.editor.reveal).toHaveBeenLastCalledWith('lib/tool.py', 3, 1) })
    expect(h.request).toHaveBeenCalledWith({ op: 'files.read', workspaceId: workspace.workspaceId, path: 'lib/tool.py' })
    act(() => { emit('editor.open', { path: '/etc/passwd' }) })
    await waitFor(() => { expect(byRole('alert').textContent).toBe('该路径不在已添加的文件夹内') })
  })

  it('compares assistant code with the open file and applies it to the buffer without saving', async () => {
    const h = await mountWindow()
    act(() => { emit('editor.snippet', { code: 'x = 2\n', language: 'py', compare: true }) })
    expect(h.workbench.model.state.getSnapshot().data.tabs.at(-1)?.kind).toBe('diff')
    click(byRole('button', '应用到文件缓冲区'))
    await waitFor(() => { expect(h.workbench.model.state.getSnapshot().buffers['main.py']).toMatchObject({ text: 'x = 2\n', dirty: true }) })
    expect(h.request.mock.calls.some(([body]) => body.op === 'files.save')).toBe(false)
  })

  it('shows panes and settings sections on request', async () => {
    const h = await mountWindow()
    act(() => { emit('pane.show', { pane: 'bottom' }) })
    expect(h.workbench.model.state.getSnapshot().data.layout.bottomVisible).toBe(true)
    act(() => { emit('pane.show', { pane: 'ctf' }) })
    expect(byRole('tab', 'CTF 工具').getAttribute('aria-selected')).toBe('true')
    act(() => { emit('pane.show', { pane: 'settings', section: 'memory' }) })
    expect(byRole('dialog', 'settings:dialog').dataset.section).toBe('memory')
    click(byRole('button', 'settings:close'))
    act(() => { emit('pane.show', { pane: 'settings', section: 'unknown' }) })
    expect(byRole('dialog', 'settings:dialog').dataset.section).toBe('general')
  })
})

describe('window shortcuts and menus', () => {
  it('opens quick open, settings, a new chat and the history search from the keyboard', async () => {
    const h = await mountWindow()
    h.request.mockResolvedValue({ paths: [], truncated: false })
    press('KeyP', { alt: true })
    expect(byRole('dialog', '搜索工作区文件')).toBeTruthy()
    click(byRole('button', '关闭'))
    press('Comma')
    expect(byRole('dialog', 'settings:dialog').dataset.section).toBe('general')
    press('Comma')
    expect(queryByRole('dialog')).toBeNull()
    click(byRole('button', '设置'))
    expect(byRole('dialog', 'settings:dialog').dataset.section).toBe('models')
    press('KeyN', { alt: true })
    expect(h.workbench.model.state.getSnapshot().data.layout.agentVisible).toBe(false)
    click(byRole('button', 'settings:close'))
    act(() => { h.workbench.showSession('chat-a') })
    press('KeyN', { alt: true })
    expect(chatState.chat?.sessionId).toBeNull()
    expect(h.workbench.model.state.getSnapshot().data.layout.agentVisible).toBe(true)
    press('KeyK', { alt: true })
    await waitFor(() => { expect(window.document.activeElement).toBe(byRole('textbox', 'history:search')) })
  })

  it('opens a folder and a terminal from the keyboard', async () => {
    const h = await mountWindow()
    press('KeyO', { alt: true })
    await waitFor(() => { expect(h.chooseDirectory).toHaveBeenCalledOnce() })
    press('Backquote')
    expect(h.workbench.model.state.getSnapshot().data.layout).toMatchObject({ bottomVisible: true, bottomTab: 'terminal' })
    await waitFor(() => { expect(h.executionRequest).toHaveBeenCalledWith(expect.objectContaining({ op: 'terminal.start', workspaceId: workspace.workspaceId }), expect.any(AbortSignal)) })
  })

  it('asks the desktop for its Help menu in the interface language', async () => {
    await mountWindow()
    const menus: unknown[] = []
    const listener = (event: Event): void => { if (event instanceof CustomEvent) menus.push(event.detail) }
    window.addEventListener('rainy:native-menu', listener)
    click(byRole('button', '帮助'))
    window.removeEventListener('rainy:native-menu', listener)
    expect(menus).toEqual([{ menu: 'help', locale: 'zh' }])
  })

  it('switches, renames and removes projects from the project menu', async () => {
    const h = await mountWindow()
    const select = vi.spyOn(h.workbench.model, 'selectWorkspace').mockResolvedValue()
    click(byRole('button', '工作区'))
    click(byRole('menuitem', 'Other'))
    expect(select).toHaveBeenCalledWith(other)
    h.request.mockResolvedValueOnce({ ...workspace, title: 'Renamed' })
    click(byRole('button', '工作区'))
    click(byRole('menuitem', '重命名项目'))
    change(byRole('textbox', '项目名称'), 'Renamed')
    click(byRole('button', '确认'))
    await waitFor(() => { expect(byRole('button', '工作区').textContent).toBe('Renamed') })
    expect(h.request).toHaveBeenCalledWith({ op: 'workspaces.rename', workspaceId: workspace.workspaceId, title: 'Renamed' })
    const remove = vi.spyOn(h.workbench.model, 'removeWorkspace').mockResolvedValue()
    click(byRole('button', '工作区'))
    click(byRole('menuitem', '从项目列表移除'))
    expect(byRole('dialog', '从项目列表移除').textContent).toContain('将从项目列表移除 Renamed，磁盘上的文件不会被删除。')
    click(byRole('button', '确认'))
    await waitFor(() => { expect(remove).toHaveBeenCalledWith(workspace.workspaceId) })
  })
})
