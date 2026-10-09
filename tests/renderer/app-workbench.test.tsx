// @vitest-environment happy-dom
/** The window keeps the chat mounted while file views, geometry and recovery state change. */
import { act } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { IdeFileVersion } from '../../src/shared/ide-files-protocol.ts'
import { IdeRequestError } from '../../src/renderer/ide/ide-api.ts'
import { emit } from '../../src/renderer/app/bus.ts'
import { allByRole, byRole, change, cleanup, click, findByRole, keyDown, queryByRole, waitFor } from './ide-dom.tsx'
import { chatState, resetChatState } from './app-chat-mock.tsx'
import { assets, disposeWindows, document as source, mountWindow } from './app-fixture.tsx'

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

function chatPane(): HTMLElement {
  const element = window.document.querySelector<HTMLElement>('[data-testid="chat-pane"]')
  if (element === null) throw new Error('The chat pane is not mounted')
  return element
}

/** @returns The directory browser's Open button once its listing has landed. */
function openEnabled(): Promise<HTMLElement> {
  return waitFor(() => {
    const button = byRole('button', '打开')
    expect(button.matches(':disabled')).toBe(false)
    return button
  })
}

describe('window layout', () => {
  it('opens optional panes and tool tabs only on demand from an empty workspace', async () => {
    const h = await mountWindow(950)
    act(() => {
      const current = h.workbench.model.state.getSnapshot()
      h.workbench.model.state.set({ ...current, buffers: {}, data: { ...current.data, tabs: [], activePath: null } })
    })
    expect(queryByRole('complementary', 'AI 助手')).toBeNull()
    expect(queryByRole('tablist')).toBeNull()
    expect(queryByRole('button', '运行：Python')).toBeNull()
    expect(byRole('tree', '文件')).toBeTruthy()
    click(byRole('button', 'CTF 工具'))
    expect(byRole('tab', 'CTF 工具')).toBeTruthy()
    click(byRole('button', '收起 CTF 工具'))
    expect(queryByRole('tablist')).toBeNull()
    click(byRole('button', '显示或隐藏 AI 助手'))
    expect(byRole('complementary', 'AI 助手').contains(chatPane())).toBe(true)
    expect(chatState).toMatchObject({ mounts: 1, unmounts: 0 })
  })

  it('shows a readonly AI preview from the chat before any project is selected', async () => {
    const h = await mountWindow()
    act(() => {
      const current = h.workbench.model.state.getSnapshot()
      h.workbench.model.state.set({ ...current, workspace: null, buffers: {}, data: { ...current.data, tabs: [], activePath: null } })
      emit('editor.snippet', { code: 'print("preview")', language: 'python', compare: false })
    })
    await waitFor(() => {
      const documents = vi.mocked(h.editor.updateDocuments).mock.calls.at(-1)?.[0]
      expect(documents).toHaveLength(1)
      expect(documents?.[0]?.uri.startsWith('untitled:')).toBe(true)
      expect(documents?.[0]?.readOnly).toBe(true)
      expect(documents?.[0]?.text).toBe('print("preview")')
    })
    expect(byRole('tab', /^AI 代码片段 1\.py/u)).toBeTruthy()
    expect(h.request).not.toHaveBeenCalled()
  })

  it('opens the Host directory browser from the file menu and adopts the listed folder', async () => {
    const h = await mountWindow()
    const adopt = vi.spyOn(h.workbench.model, 'openWorkspace').mockResolvedValue()
    click(byRole('button', '文件'))
    click(byRole('menuitem', '浏览当前执行环境文件夹'))
    await findByRole('dialog', '选择工作区目录')
    await waitFor(() => { expect(h.listDirectory).toHaveBeenCalledWith(undefined, expect.any(AbortSignal)) })
    click(await openEnabled())
    await waitFor(() => { expect(adopt).toHaveBeenCalledWith('/tmp/project') })
    expect(h.chooseDirectory).not.toHaveBeenCalled()
    await waitFor(() => { expect(queryByRole('dialog')).toBeNull() })
  })

  it('opens a project from the Host browser after the native add-folder action', async () => {
    const h = await mountWindow()
    const open = vi.spyOn(h.workbench.model, 'openWorkspace').mockResolvedValue()
    const attach = vi.spyOn(h.workbench.model, 'attachRoot').mockResolvedValue()
    click(byRole('button', '添加文件夹到工作区'))
    await waitFor(() => { expect(h.chooseDirectory).toHaveBeenCalledOnce() })
    await waitFor(() => { expect(h.workbench.directory.pending.getSnapshot()).toBe(false) })
    click(byRole('button', '文件'))
    click(byRole('menuitem', '浏览当前执行环境文件夹'))
    await findByRole('dialog', '选择工作区目录')
    click(await openEnabled())
    await waitFor(() => { expect(open).toHaveBeenCalledWith('/tmp/project') })
    expect(attach).not.toHaveBeenCalled()
  })

  it('attaches the natively chosen folder to the current project', async () => {
    const h = await mountWindow()
    const attach = vi.spyOn(h.workbench.model, 'attachRoot').mockResolvedValue()
    h.chooseDirectory.mockResolvedValueOnce({ path: '/shared', displayPath: '/shared' })
    click(byRole('button', '添加文件夹到工作区'))
    await waitFor(() => { expect(attach).toHaveBeenCalledExactlyOnceWith('/shared') })
  })

  it('disables folder actions while a native choice owns the workspace operation', async () => {
    const h = await mountWindow()
    const chosen = Promise.withResolvers<{ path: string; displayPath: string } | null>()
    h.chooseDirectory.mockReturnValueOnce(chosen.promise)
    const add = byRole('button', '添加文件夹到工作区')
    click(add)
    click(add)
    click(add)
    await waitFor(() => { expect(h.chooseDirectory).toHaveBeenCalledOnce() })
    expect(add.matches(':disabled')).toBe(true)
    click(byRole('button', '文件'))
    const state = (name: string): string => byRole('menuitem', name).matches(':disabled') ? 'disabled' : 'enabled'
    expect([state('打开 Windows 文件夹'), state('浏览当前执行环境文件夹')]).toEqual(['disabled', 'disabled'])
    keyDown(window.document, { key: 'Escape' })
    click(byRole('button', '文件操作'))
    expect([state('打开文件夹'), state('添加文件夹到工作区')]).toEqual(['disabled', 'disabled'])
    keyDown(window.document, { key: 'Escape' })
    await act(async () => { chosen.resolve(null); await chosen.promise })
    await waitFor(() => { expect(add.matches(':disabled')).toBe(false) })
    click(add)
    await waitFor(() => { expect(h.chooseDirectory).toHaveBeenCalledTimes(2) })
  })

  it('searches unopened workspace filenames through the quick-open dialog', async () => {
    const h = await mountWindow()
    h.request.mockResolvedValueOnce({ paths: ['lib/unopened.py'], truncated: false })
    click(byRole('button', '搜索工作区文件'))
    await findByRole('option', 'lib/unopened.py')
    expect(h.request).toHaveBeenCalledWith({ op: 'files.search', workspaceId: 'a', query: '' }, expect.any(AbortSignal))
    h.request.mockResolvedValueOnce({ ...source, path: 'lib/unopened.py', content: 'opened source' })
    click(byRole('option', 'lib/unopened.py'))
    await waitFor(() => { expect(h.workbench.model.state.getSnapshot().data.activePath).toBe('lib/unopened.py') })
    expect(queryByRole('dialog')).toBeNull()
  })

  it('offers workspace file operations without requiring a chat', async () => {
    const h = await mountWindow()
    click(byRole('button', '文件'))
    expect(byRole('menu').textContent).toBe('打开 Windows 文件夹浏览当前执行环境文件夹搜索工作区文件新建文件新建文件夹保存全部保存运行配置')
    click(byRole('menuitem', '打开 Windows 文件夹'))
    await waitFor(() => { expect(h.chooseDirectory).toHaveBeenCalledOnce() })
    expect(queryByRole('menu')).toBeNull()
  })

  it('disables the Windows folder action and keeps browsing without the desktop picker', async () => {
    await mountWindow(1440, { native: false })
    click(byRole('button', '文件'))
    expect(byRole('menuitem', '打开 Windows 文件夹').matches(':disabled')).toBe(true)
    expect(byRole('menuitem', '浏览当前执行环境文件夹').matches(':disabled')).toBe(false)
    keyDown(window.document, { key: 'Escape' })
    click(byRole('button', '添加文件夹到工作区'))
    await findByRole('dialog', '选择工作区目录')
  })

  it('keeps the same chat and draft while resizing, switching tool tabs, and hiding the AI pane', async () => {
    const h = await mountWindow()
    click(byRole('button', '显示或隐藏 AI 助手'))
    const chat = byRole('textbox', 'chat:draft') as HTMLInputElement
    change(chat, 'unfinished chat draft')
    click(byRole('button', 'CTF 工具'))
    h.rerender(950)
    click(byRole('button', '视图'))
    click(byRole('menuitem', '专注编辑'))
    click(byRole('button', '显示或隐藏 AI 助手'))
    click(byRole('tab', 'main.py'))
    expect(byRole('textbox', 'chat:draft')).toBe(chat)
    expect(chat.value).toBe('unfinished chat draft')
    expect(chatState).toMatchObject({ mounts: 1, unmounts: 0 })
    expect(assets.create).toHaveBeenCalledOnce()
    expect(h.editor.dispose).not.toHaveBeenCalled()
  })

  it('bounds keyboard and stored bottom panel sizes like pointer resizing', async () => {
    const h = await mountWindow()
    const limit = 900 - 230
    act(() => { h.workbench.model.layout({ bottomVisible: true, bottomHeight: limit + 500 }) })
    const separator = byRole('separator', '调整底部面板高度')
    const panel = separator.parentElement as HTMLElement
    expect(panel.style.height).toBe(`${limit}px`)
    for (let press = 0; press < 40; press++) keyDown(separator, { key: 'ArrowUp' })
    expect(h.workbench.model.state.getSnapshot().data.layout.bottomHeight).toBe(limit)
    keyDown(separator, { key: 'ArrowDown' })
    expect(h.workbench.model.state.getSnapshot().data.layout.bottomHeight).toBe(limit - 16)
  })

  it('bounds the file and AI pane widths', async () => {
    const h = await mountWindow(1440)
    click(byRole('button', '显示或隐藏 AI 助手'))
    const files = byRole('separator', '调整文件栏宽度')
    for (let press = 0; press < 20; press++) keyDown(files, { key: 'ArrowRight' })
    expect(h.workbench.model.state.getSnapshot().data.layout.sidebarWidth).toBe(360)
    for (let press = 0; press < 20; press++) keyDown(files, { key: 'ArrowLeft' })
    expect(h.workbench.model.state.getSnapshot().data.layout.sidebarWidth).toBe(180)
    const agent = byRole('separator', '调整 AI 助手宽度')
    for (let press = 0; press < 80; press++) keyDown(agent, { key: 'ArrowLeft' })
    expect(h.workbench.model.state.getSnapshot().data.layout.agentWidth).toBe(1440 - 180 - 300)
    const shell = h.view.container.querySelector<HTMLElement>('[data-rainy-ide]')
    expect(shell?.style.gridTemplateColumns).toBe('180px minmax(0, 1fr) 960px')
  })

  it('collapses files at 950 px and can explicitly reveal history without replacing chat', async () => {
    const h = await mountWindow(950)
    click(byRole('button', '显示或隐藏 AI 助手'))
    const left = h.view.container.querySelector<HTMLElement>('aside[aria-label="工作区"]')
    const shell = h.view.container.querySelector<HTMLElement>('[data-rainy-ide]')
    expect(left?.hidden).toBe(true)
    expect(shell?.style.gridTemplateColumns).toBe('0px minmax(0, 1fr) 400px')
    click(byRole('button', 'chat:history'))
    expect(left?.hidden).toBe(false)
    expect(left?.style.position).toBe('absolute')
    expect(shell?.style.gridTemplateColumns).toBe('0px minmax(0, 1fr) 400px')
    expect(byRole('textbox', 'history:search')).toBeTruthy()
    expect(chatState.mounts).toBe(1)
  })

  it('does not carry wide history navigation into the next narrow viewport', async () => {
    const h = await mountWindow(1380)
    click(byRole('button', '显示或隐藏 AI 助手'))
    const left = h.view.container.querySelector<HTMLElement>('aside[aria-label="工作区"]')
    act(() => { emit('pane.show', { pane: 'history' }) })
    click(byRole('button', '返回文件'))
    h.rerender(950)
    expect(left?.hidden).toBe(true)
    act(() => { emit('pane.show', { pane: 'history' }) })
    expect(left?.hidden).toBe(false)
    h.rerender(1380)
    h.rerender(950)
    expect(left?.hidden).toBe(true)
    expect(chatState).toMatchObject({ mounts: 1, unmounts: 0 })
  })

  it('does not launch a program when any dirty source conflicts with disk', async () => {
    const h = await mountWindow()
    const launch = vi.spyOn(h.workbench.execution, 'run').mockResolvedValue()
    act(() => {
      const current = h.workbench.model.state.getSnapshot()
      h.workbench.model.state.set({ ...current, buffers: { ...current.buffers,
        'helper.py': { document: { ...source, path: 'helper.py' }, text: 'helper changed', dirty: true, external: false } } })
      h.workbench.model.change('main.py', 'main changed')
    })
    h.request
      .mockResolvedValueOnce({ ...source, content: 'main changed', version: 'v2' as IdeFileVersion })
      .mockRejectedValueOnce(new IdeRequestError('version-conflict', 'helper changed on disk'))
      .mockResolvedValueOnce({ ...source, path: 'helper.py', content: 'other helper', version: 'v2' as IdeFileVersion })
    click(byRole('button', '运行：Python'))
    await waitFor(() => { expect(byRole('alert').textContent).toContain('helper changed on disk') })
    expect(h.request.mock.calls.filter(([body]) => body.op === 'files.save').map(([body]) => ('path' in body ? body.path : '')))
      .toEqual(['main.py', 'helper.py'])
    expect(launch).not.toHaveBeenCalled()
    expect(h.workbench.model.state.getSnapshot().buffers['helper.py']?.text).toBe('helper changed')
  })

  it('runs the open file by its extension or with a language chosen from the run menu', async () => {
    const h = await mountWindow()
    const launch = vi.spyOn(h.workbench.execution, 'run').mockResolvedValue()
    click(byRole('button', '运行方式'))
    click(await findByRole('menuitem', 'C++'))
    click(byRole('button', '运行：C++'))
    await waitFor(() => { expect(launch).toHaveBeenCalledOnce() })
    expect(launch.mock.calls[0]?.[0]).toMatchObject({ program: 'main.py', language: 'cpp' })
    expect(h.workbench.model.state.getSnapshot().data.execution?.activeProfile).toBeNull()
    expect(h.workbench.model.state.getSnapshot().data.layout).toMatchObject({ bottomVisible: true, bottomTab: 'terminal' })
    click(byRole('button', '运行方式'))
    click(await findByRole('menuitem', '按后缀自动识别（Python）'))
    expect(byRole('button', '运行：Python')).toBeTruthy()
    expect(h.workbench.model.state.getSnapshot().data.execution?.profiles).toEqual([])
  })

  it('offers an explicit dirty close choice and keeps the buffer when cancelled', async () => {
    const h = await mountWindow()
    act(() => { h.workbench.model.change('main.py', 'unsaved') })
    click(byRole('button', '关闭 main.py'))
    expect(byRole('dialog', '文件有未保存的修改')).toBeTruthy()
    click(byRole('button', '取消'))
    expect(h.workbench.model.state.getSnapshot().buffers['main.py']?.text).toBe('unsaved')
    expect(h.request.mock.calls.some(([body]) => body.op === 'files.save')).toBe(false)
    click(byRole('button', '关闭 main.py'))
    click(byRole('button', '放弃修改'))
    await waitFor(() => { expect(h.workbench.model.state.getSnapshot().data.tabs).toEqual([]) })
  })

  it('retains the active file when an interpreter restart follows an earlier debug reveal', async () => {
    const h = await mountWindow()
    vi.spyOn(h.workbench.model, 'flush').mockResolvedValue(true)
    const { reveal, setWorkspace, show } = vi.mocked(h.editor)
    await act(async () => { await h.workbench.model.reveal('main.py', 2, 1) })
    expect(reveal).toHaveBeenLastCalledWith('main.py', 2, 1)
    const revealed = reveal.mock.calls.length
    act(() => {
      const previous = h.workbench.model.state.getSnapshot()
      h.workbench.model.state.set({ ...previous,
        data: { ...previous.data, activePath: 'venv.py', tabs: [...previous.data.tabs, { path: 'venv.py', kind: 'file' }] },
        buffers: { ...previous.buffers, 'venv.py': { document: { ...source, path: 'venv.py' }, text: source.content, dirty: false, external: false } },
      })
      h.workbench.model.execution({ profiles: [{ name: 'Project Python', language: 'python', program: 'venv.py',
        executable: '/tmp/project/env/bin/python' }], activeProfile: 'Project Python', breakpoints: [], watches: [] })
    })
    await waitFor(() => {
      expect(setWorkspace).toHaveBeenLastCalledWith(expect.objectContaining({ pythonPath: '/tmp/project/env/bin/python' }))
      expect(show).toHaveBeenLastCalledWith('venv.py', undefined)
    })
    expect(reveal.mock.calls).toHaveLength(revealed)
    await act(async () => { await h.workbench.model.openFile('main.py') })
    expect(reveal.mock.calls).toHaveLength(revealed)
    await act(async () => { await h.workbench.model.reveal('main.py', 4, 1) })
    expect(reveal).toHaveBeenLastCalledWith('main.py', 4, 1)
    expect(reveal.mock.calls).toHaveLength(revealed + 1)
  })

  it('shows status bar details for the caret and the open document', async () => {
    const h = await mountWindow()
    act(() => {
      h.workbench.model.selection({ path: 'main.py', text: 'x = 1', language: 'python', startLine: 1, startColumn: 1, endLine: 1, endColumn: 6 })
    })
    const status = h.view.container.querySelector('[data-rainy-status]')
    expect([...status?.children ?? []].map(item => item.textContent).filter(text => text !== '')).toEqual(
      ['行 1，列 6', '已选 5 个字符', 'python', 'LF', 'UTF-8'])
    expect(allByRole('alert')).toHaveLength(0)
  })
})
