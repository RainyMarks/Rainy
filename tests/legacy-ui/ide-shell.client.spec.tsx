// @vitest-environment jsdom
/** IDE composition retains chat while file views, geometry, and recovery state change. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { useEffect, useState, type ComponentProps } from 'react'
import { bindSnapshotSelector } from '@deepseek-ai/dsh-client-test-runtime'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import { IdeShell } from '../src/client/IdeShell.tsx'
import { IdeModel } from '../src/client/ide-model.ts'
import { IdeDirectorySelection } from '../src/client/ide-directory-selection.ts'
import { IdeExecutionModel } from '../src/client/ide-execution-model.ts'
import { IdeRequestError, type IdeFilesApi } from '../src/client/ide-api.ts'
import type { EditorAppearance, EditorAssets, EditorInstance } from '../src/client/editor-types.ts'
import type { IdeFileVersion, WorkspaceId } from '../src/ide-files-protocol.ts'
import type { LayoutRegionInput } from '@deepseek-ai/dsh-client-ui-layout/client'
import type { DirectoryFlowOwnerProps } from '@deepseek-ai/dsh-client-ui-workspace/client'
import { BrowseDirectoryFlow } from '@deepseek-ai/dsh-client-ui-directory-picker-browse/src/client/flow.ts'
import { zh } from '../src/client/locales.ts'
import { globalProps } from './global-props.client.ts'

const assets = vi.hoisted(() => ({ create: vi.fn<EditorAssets['create']>() }))
vi.mock('../src/client/editor-loader.ts', () => ({ loadEditorAssets: async () => assets }))
const owners: { model: IdeModel; execution: IdeExecutionModel; directory: IdeDirectorySelection }[] = []
afterEach(async () => {
  cleanup()
  for (const owner of owners.splice(0)) {
    owner.directory.dispose()
    owner.model.dispose()
    await owner.execution.dispose()
  }
  vi.clearAllMocks()
})
const t = ((key: keyof typeof zh, values?: Record<string, string | number>) =>
  zh[key].replace(/\{(\w+)\}/g, (match, name: string) =>
    values?.[name] === undefined ? match : String(values[name]),
  )) as TranslateNS<'rainy'>

async function fixture(width = 1440) {
  const request = vi.fn<IdeFilesApi['request']>()
  const model = new IdeModel(
    { request: request as IdeFilesApi['request'] },
    { debounceMs: 60_000, pollMs: 60_000, restoreSession: vi.fn(async () => {}) },
  )
  const workspace = { workspaceId: 'a' as WorkspaceId, path: '/tmp/project', title: 'Project' }
  const document = {
    workspaceId: workspace.workspaceId,
    path: 'main.py',
    version: 'v1' as IdeFileVersion,
    bytes: 7,
    content: 'x = 1\n',
    bom: false,
    eol: 'lf' as const,
    readOnlyReason: null,
  }
  model.state.set({
    ...model.state.getSnapshot(),
    workspace,
    workspaces: [workspace],
    phase: 'ready',
    data: { ...model.state.getSnapshot().data, tabs: [{ path: 'main.py', kind: 'file' }], activePath: 'main.py' },
    buffers: { 'main.py': { document, text: document.content, dirty: false, external: false } },
  })
  const execution = new IdeExecutionModel(
    { request: vi.fn() },
    {
      pollMs: 60_000,
      activePollMs: 60_000,
      maxOutputCharacters: 100_000,
      maxRetainedWorkspaces: 8,
      terminalCols: 80,
      terminalRows: 24,
      getConfiguration: () =>
        model.state.getSnapshot().data.execution ?? { profiles: [], activeProfile: null, breakpoints: [], watches: [] },
      setConfiguration: (config) => {
        model.execution(config)
      },
      onError: (error) => {
        model.fail(error)
      },
      onReveal: vi.fn(),
    },
  )
  const chooseDirectory = vi.fn<() => Promise<string | null>>().mockResolvedValue(null)
  const directory = new IdeDirectorySelection({ choose: chooseDirectory,
    adopt: (path, mode) => mode === 'attach' ? model.attachRoot(path) : model.openWorkspace(path) })
  owners.push({ model, execution, directory })
  const editor: EditorInstance = {
    setWorkspace: vi.fn(async () => {}),
    updateDocuments: vi.fn(),
    show: vi.fn(),
    showDiff: vi.fn(),
    setAppearance: vi.fn(),
    setBreakpoints: vi.fn(),
    reveal: vi.fn(),
    action: vi.fn(async () => {}),
    layout: vi.fn(),
    dispose: vi.fn(async () => {}),
  }
  assets.create.mockResolvedValue(editor)
  const setWorkspace = vi.spyOn(editor, 'setWorkspace')
  const dispose = vi.spyOn(editor, 'dispose')
  const updateDocuments = vi.spyOn(editor, 'updateDocuments')
  let chatMounts = 0
  let chatUnmounts = 0
  const listDirectory = vi.fn(async () => ({ path: '/tmp/project', home: '/tmp/project',
    crumbs: [{ name: 'Project', path: '/tmp/project', hidden: false }], entries: [], truncated: false }))
  function Conversation() {
    const [draft, setDraft] = useState('')
    useEffect(() => {
      chatMounts++
      return () => {
        chatUnmounts++
      }
    }, [])
    return (
      <input
        aria-label="Real chat occurrence"
        value={draft}
        onChange={(event) => {
          setDraft(event.target.value)
        }}
      />
    )
  }
  const props: ComponentProps<typeof IdeShell> = {
    ...globalProps,
    viewportWidth: width,
    sidebarWidth: 240,
    sidebarCollapsed: false,
    auxiliaryWidth: 0,
    auxiliaryCanShow: false,
    t,
    model,
    execution,
    useIde: bindSnapshotSelector(model.state),
    useExecution: bindSnapshotSelector(execution.state),
    useQuickOpenShortcut: selector => selector(['Ctrl', '+', 'P']),
    useDirectoryPending: bindSnapshotSelector(directory.pending),
    useAppearance: bindSnapshotSelector(createSnapshotStore<EditorAppearance>({ dark: true, fontSize: 13 })),
    openFolder: vi.fn((mode?: 'open' | 'attach') => directory.open(mode)),
    nativeDirectory: true,
    newChat: vi.fn(),
    settings: vi.fn(),
    sendSelection: vi.fn(async () => {}),
    renderFactorySlot: (name, raw) => {
      if (name === 'workspace.directoryBrowser') return <BrowseDirectoryFlow {...raw as DirectoryFlowOwnerProps}
        listDirectory={listDirectory} createDirectory={async (path, name) => `${path}/${name}`} t={key => key} />
      if (name !== 'layout.region') throw new Error(`Unexpected factory ${name}`)
      const input = raw as LayoutRegionInput
      return input.region === 'conversation' ? <Conversation /> : <div data-testid={`${input.region}-occurrence`} />
    },
    renderSlot: () => <div data-testid="tool-occurrence" />,
  }
  const view = render(<IdeShell {...props} />)
  await waitFor(() => {
    expect(setWorkspace).toHaveBeenCalledOnce()
  })
  return {
    model,
    request,
    execution,
    editor,
    dispose,
    updateDocuments,
    listDirectory,
    directory,
    chooseDirectory,
    view,
    props,
    document,
    counts: () => ({ chatMounts, chatUnmounts }),
  }
}

describe('IDE shell', () => {
  it('opens optional panes and tool tabs only on demand from an empty workspace', async () => {
    const h = await fixture(950)
    act(() => {
      const current = h.model.state.getSnapshot()
      h.model.state.set({ ...current, buffers: {}, data: { ...current.data, tabs: [], activePath: null } })
    })
    expect(screen.queryByRole('complementary', { name: 'AI 助手' })).toBeNull()
    expect(screen.queryByRole('tablist')).toBeNull()
    expect(screen.queryByRole('button', { name: '运行' })).toBeNull()
    expect(screen.getByRole('tree', { name: '文件' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: 'CTF 工具' }))
    expect(screen.getByRole('tab', { name: 'CTF 工具' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: zh.ctfClose }))
    expect(screen.queryByRole('tablist')).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '显示或隐藏 AI 助手' }))
    expect(screen.getByRole('textbox', { name: 'Real chat occurrence' })).toBeTruthy()
    expect(h.counts()).toEqual({ chatMounts: 1, chatUnmounts: 0 })
  })
  it('shows a readonly AI preview before any editor workspace is selected', async () => {
    const h = await fixture()
    act(() => {
      const current = h.model.state.getSnapshot()
      h.model.state.set({ ...current, workspace: null, buffers: {}, data: { ...current.data, tabs: [], activePath: null } })
      h.model.openSnippet('print("preview")', 'python', 'AI snippet', false)
    })
    await waitFor(() => {
      const documents = h.updateDocuments.mock.calls.at(-1)?.[0]
      expect(documents).toHaveLength(1)
      expect(documents?.[0]?.uri.startsWith('untitled:')).toBe(true)
      expect(documents?.[0]?.readOnly).toBe(true)
      expect(documents?.[0]?.text).toBe('print("preview")')
    })
    expect(h.request).not.toHaveBeenCalled()
    expect(h.counts()).toEqual({ chatMounts: 1, chatUnmounts: 0 })
  })

  it('opens the current Host directory browser from the file menu', async () => {
    const h = await fixture()
    const adopt = vi.spyOn(h.model, 'openWorkspace').mockResolvedValue()
    fireEvent.click(screen.getByRole('button', { name: '文件' }))
    fireEvent.click(screen.getByRole('menuitem', { name: '浏览当前执行环境文件夹' }))
    await screen.findByRole('dialog', { name: 'browser.title' })
    await waitFor(() => { expect(h.listDirectory).toHaveBeenCalledWith(undefined, expect.any(AbortSignal)) })
    fireEvent.click(screen.getByRole('button', { name: 'browser.open' }))
    await waitFor(() => { expect(adopt).toHaveBeenCalledWith('/tmp/project') })
    expect(h.props.openFolder).not.toHaveBeenCalled()
    await waitFor(() => { expect(screen.queryByRole('dialog')).toBeNull() })
  })

  it('opens a project from the Host browser after the native add-folder action', async () => {
    const h = await fixture()
    const open = vi.spyOn(h.model, 'openWorkspace').mockResolvedValue()
    const attach = vi.spyOn(h.model, 'attachRoot').mockResolvedValue()
    fireEvent.click(screen.getByRole('button', { name: zh.ideAddFolder }))
    expect(h.props.openFolder).toHaveBeenCalledWith('attach')
    await waitFor(() => { expect(h.directory.pending.getSnapshot()).toBe(false) })
    fireEvent.click(screen.getByRole('button', { name: '文件' }))
    fireEvent.click(screen.getByRole('menuitem', { name: zh.ideWslFolder }))
    await screen.findByRole('dialog', { name: 'browser.title' })
    fireEvent.click(screen.getByRole('button', { name: 'browser.open' }))
    await waitFor(() => { expect(open).toHaveBeenCalledWith('/tmp/project') })
    expect(attach).not.toHaveBeenCalled()
  })

  it('disables folder actions while a native choice owns the workspace operation', async () => {
    const h = await fixture()
    const chosen = Promise.withResolvers<string | null>()
    h.chooseDirectory.mockReturnValueOnce(chosen.promise)
    const add = screen.getByRole('button', { name: zh.ideAddFolder })
    fireEvent.click(add)
    fireEvent.click(add)
    fireEvent.click(add)
    expect(h.props.openFolder).toHaveBeenCalledExactlyOnceWith('attach')
    expect(add.matches(':disabled')).toBe(true)
    await waitFor(() => { expect(h.chooseDirectory).toHaveBeenCalledOnce() })
    fireEvent.click(screen.getByRole('button', { name: '文件' }))
    const describe = (name: string) => {
      const item = screen.getByRole('menuitem', { name })
      return `${item.textContent}: ${item.matches(':disabled,[aria-disabled="true"]') ? 'disabled' : 'enabled'}`
    }
    const file = [zh.ideWindowsFolder, zh.ideWslFolder].map(describe)
    fireEvent.keyDown(document, { key: 'Escape' })
    fireEvent.click(screen.getByRole('button', { name: zh.ideFileActions }))
    const explorer = [zh.ideOpenFolder, zh.ideAddFolder].map(describe)
    await expect(['File', ...file, 'Explorer', ...explorer, ''].join('\n'))
      .toMatchFileSnapshot('./expected/pending-directory-menu.txt')
    fireEvent.keyDown(document, { key: 'Escape' })
    const completion = h.directory.open('open')
    await act(async () => { chosen.resolve(null); await completion })
    expect(add.matches(':disabled')).toBe(false)
    fireEvent.click(add)
    await waitFor(() => { expect(h.chooseDirectory).toHaveBeenCalledTimes(2) })
    await waitFor(() => { expect(h.directory.pending.getSnapshot()).toBe(false) })
  })

  it('searches unopened workspace filenames through the quick-open dialog', async () => {
    const h = await fixture()
    h.request.mockResolvedValueOnce({ paths: ['lib/unopened.py'], truncated: false })
    fireEvent.click(screen.getByRole('button', { name: '搜索工作区文件' }))
    await screen.findByRole('option', { name: 'lib/unopened.py' })
    expect(h.request).toHaveBeenCalledWith({ op: 'files.search', workspaceId: 'a', query: '' }, expect.any(AbortSignal))
    h.request.mockResolvedValueOnce({ ...h.document, path: 'lib/unopened.py', content: 'opened source' })
    fireEvent.click(screen.getByRole('option', { name: 'lib/unopened.py' }))
    await waitFor(() => { expect(h.model.state.getSnapshot().data.activePath).toBe('lib/unopened.py') })
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('offers workspace file operations without requiring a chat', async () => {
    const h = await fixture()
    fireEvent.click(screen.getByRole('button', { name: '文件' }))
    const menu = screen.getByRole('menu')
    await expect(menu.textContent + '\n').toMatchFileSnapshot('./expected/file-menu.txt')
    fireEvent.click(screen.getByRole('menuitem', { name: '打开 Windows 文件夹' }))
    expect(h.props.openFolder).toHaveBeenCalledOnce()
    expect(screen.queryByRole('menu')).toBeNull()
  })
  it('keeps the same chat and draft while resizing, switching tool tabs, and hiding the AI pane', async () => {
    const h = await fixture()
    fireEvent.click(screen.getByRole('button', { name: '显示或隐藏 AI 助手' }))
    const chat = screen.getByRole('textbox', { name: 'Real chat occurrence' })
    fireEvent.change(chat, { target: { value: 'unfinished chat draft' } })
    fireEvent.click(screen.getByRole('button', { name: 'CTF 工具' }))
    h.view.rerender(<IdeShell {...h.props} viewportWidth={950} />)
    fireEvent.click(screen.getByRole('button', { name: '视图' }))
    fireEvent.click(screen.getByRole('menuitem', { name: '专注编辑' }))
    fireEvent.click(screen.getByRole('button', { name: '显示或隐藏 AI 助手' }))
    fireEvent.click(screen.getByRole('tab', { name: 'main.py' }))
    expect(screen.getByRole('textbox', { name: 'Real chat occurrence' })).toBe(chat)
    expect((chat as HTMLInputElement).value).toBe('unfinished chat draft')
    expect(h.counts()).toEqual({ chatMounts: 1, chatUnmounts: 0 })
    expect(assets.create).toHaveBeenCalledOnce()
    expect(h.dispose).not.toHaveBeenCalled()
  })

  it('bounds keyboard and stored bottom panel sizes like pointer resizing', async () => {
    const h = await fixture()
    const limit = window.innerHeight - 230
    act(() => { h.model.layout({ bottomVisible: true, bottomHeight: limit + 500 }) })
    const separator = screen.getByRole('separator', { name: zh.ideResizeBottom })
    const panel = separator.parentElement as HTMLElement
    expect(panel.style.height).toBe(`${limit}px`)
    for (let press = 0; press < 40; press++) fireEvent.keyDown(separator, { key: 'ArrowUp' })
    expect(h.model.state.getSnapshot().data.layout.bottomHeight).toBe(limit)
    fireEvent.keyDown(separator, { key: 'ArrowDown' })
    expect(h.model.state.getSnapshot().data.layout.bottomHeight).toBe(limit - 16)
  })

  it('collapses files at 950 px and can explicitly reveal history without replacing chat', async () => {
    const h = await fixture(950)
    fireEvent.click(screen.getByRole('button', { name: '显示或隐藏 AI 助手' }))
    const left = h.view.container.querySelector<HTMLElement>('aside[aria-label="工作区"]')!
    const shell = h.view.container.querySelector<HTMLElement>('[data-rainy-ide]')!
    expect(left.hidden).toBe(true)
    expect(shell.style.gridTemplateColumns).toBe('0px minmax(0, 1fr) 400px')
    fireEvent.click(screen.getByRole('button', { name: '对话历史' }))
    expect(left.hidden).toBe(false)
    expect(left.style.position).toBe('absolute')
    expect(shell.style.gridTemplateColumns).toBe('0px minmax(0, 1fr) 400px')
    expect(h.counts().chatMounts).toBe(1)
  })

  it('does not carry wide history navigation into the next narrow viewport', async () => {
    const h = await fixture(1380)
    fireEvent.click(screen.getByRole('button', { name: '显示或隐藏 AI 助手' }))
    const left = h.view.container.querySelector<HTMLElement>('aside[aria-label="工作区"]')!
    fireEvent.click(screen.getByRole('button', { name: '对话历史' }))
    fireEvent.click(screen.getByRole('button', { name: '返回文件' }))
    h.view.rerender(<IdeShell {...h.props} viewportWidth={950} />)
    expect(left.hidden).toBe(true)
    fireEvent.click(screen.getByRole('button', { name: '对话历史' }))
    expect(left.hidden).toBe(false)
    h.view.rerender(<IdeShell {...h.props} viewportWidth={1380} />)
    h.view.rerender(<IdeShell {...h.props} viewportWidth={950} />)
    expect(left.hidden).toBe(true)
    expect(h.counts()).toEqual({ chatMounts: 1, chatUnmounts: 0 })
  })

  it('does not launch a program when any dirty source conflicts with disk', async () => {
    const h = await fixture()
    const launch = vi.spyOn(h.execution, 'run').mockResolvedValue()
    act(() => {
      const current = h.model.state.getSnapshot()
      h.model.state.set({
        ...current,
        buffers: {
          ...current.buffers,
          'helper.py': {
            document: { ...h.document, path: 'helper.py' },
            text: 'helper changed',
            dirty: true,
            external: false,
          },
        },
      })
      h.model.change('main.py', 'main changed')
    })
    h.request
      .mockResolvedValueOnce({ ...h.document, content: 'main changed', version: 'v2' as IdeFileVersion })
      .mockRejectedValueOnce(new IdeRequestError('version-conflict', 'helper changed on disk'))
      .mockResolvedValueOnce({
        ...h.document,
        path: 'helper.py',
        content: 'other helper',
        version: 'v2' as IdeFileVersion,
      })
    fireEvent.click(screen.getByRole('button', { name: '运行：Python' }))
    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('helper changed on disk')
    })
    expect(
      h.request.mock.calls
        .filter(([request]) => request.op === 'files.save')
        .map(([request]) => ('path' in request ? request.path : '')),
    ).toEqual(['main.py', 'helper.py'])
    expect(launch).not.toHaveBeenCalled()
    expect(h.model.state.getSnapshot().buffers['helper.py']?.text).toBe('helper changed')
  })

  it('runs the open file by its extension or with a language chosen from the run menu', async () => {
    const h = await fixture()
    const launch = vi.spyOn(h.execution, 'run').mockResolvedValue()
    fireEvent.click(screen.getByRole('button', { name: '运行方式' }))
    fireEvent.click(await screen.findByRole('menuitem', { name: 'C++' }))
    fireEvent.click(screen.getByRole('button', { name: '运行：C++' }))
    await waitFor(() => { expect(launch).toHaveBeenCalledOnce() })
    expect(launch.mock.calls[0]?.[0]).toMatchObject({ program: 'main.py', language: 'cpp' })
    expect(h.model.state.getSnapshot().data.execution?.activeProfile).toBeNull()
    fireEvent.click(screen.getByRole('button', { name: '运行方式' }))
    fireEvent.click(await screen.findByRole('menuitem', { name: '按后缀自动识别（Python）' }))
    expect(screen.getByRole('button', { name: '运行：Python' })).toBeTruthy()
    expect(h.model.state.getSnapshot().data.execution?.profiles).toEqual([])
  })

  it('offers an explicit dirty close choice and keeps the buffer when cancelled', async () => {
    const h = await fixture()
    act(() => {
      h.model.change('main.py', 'unsaved')
    })
    fireEvent.click(screen.getByRole('button', { name: '关闭 main.py' }))
    expect(screen.getByRole('dialog', { name: '文件有未保存的修改' })).toBeTruthy()
    fireEvent.click(screen.getByRole('button', { name: '取消' }))
    expect(h.model.state.getSnapshot().buffers['main.py']?.text).toBe('unsaved')
    expect(h.request.mock.calls.some(([request]) => request.op === 'files.save')).toBe(false)
  })

  it('retains the active file when an interpreter restart follows an earlier debug reveal', async () => {
    const h = await fixture()
    vi.spyOn(h.model, 'flush').mockResolvedValue(true)
    const reveal = vi.spyOn(h.editor, 'reveal')
    const setWorkspace = vi.spyOn(h.editor, 'setWorkspace')
    const show = vi.spyOn(h.editor, 'show')
    await act(async () => { await h.model.reveal('main.py', 2, 1) })
    expect(reveal).toHaveBeenLastCalledWith('main.py', 2, 1)
    const revealed = reveal.mock.calls.length
    act(() => {
      const previous = h.model.state.getSnapshot()
      h.model.state.set({ ...previous,
        data: { ...previous.data, activePath: 'venv.py', tabs: [...previous.data.tabs, { path: 'venv.py', kind: 'file' }] },
        buffers: { ...previous.buffers, 'venv.py': { document: { ...h.document, path: 'venv.py' },
          text: h.document.content, dirty: false, external: false } },
      })
      h.model.execution({ profiles: [{ name: 'Project Python', language: 'python', program: 'venv.py',
        executable: '/tmp/project/env/bin/python' }], activeProfile: 'Project Python', breakpoints: [], watches: [] })
    })
    await waitFor(() => {
      expect(setWorkspace).toHaveBeenLastCalledWith(expect.objectContaining({ pythonPath: '/tmp/project/env/bin/python' }))
      expect(show).toHaveBeenLastCalledWith('venv.py', undefined)
    })
    expect(reveal.mock.calls).toHaveLength(revealed)
    await act(async () => { await h.model.openFile('main.py') })
    expect(reveal.mock.calls).toHaveLength(revealed)
    await act(async () => { await h.model.reveal('main.py', 4, 1) })
    expect(reveal).toHaveBeenLastCalledWith('main.py', 4, 1)
    expect(reveal.mock.calls).toHaveLength(revealed + 1)
  })
})
