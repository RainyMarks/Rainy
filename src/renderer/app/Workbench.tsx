/** The window: top bar, file pane, editor area with the CTF workbench, AI pane, bottom panel and status bar. */
import { useEffect, useMemo, useRef, useState } from 'react'
import type { PointerEvent, ReactNode } from 'react'
import type { IdeFileEntry, IdeRootId } from '../../shared/ide-files-protocol.ts'
import type { IdeRunConfiguration } from '../../shared/ide-execution-protocol.ts'
import {
  Button, FileTypeIcon, IconAction, IconBugOutline, IconCheckOutlineRegular, IconChevronDownOutlineRegular,
  IconChevronLeftOutlineRegular, IconCloseOutlineRegular, IconEllipsisOutlineRegular, IconFolderCloseRegular,
  IconPanelBottomOutline, IconPanelLeftOutlineRegular, IconPanelRightOutline, IconPlayOutlineRegular, IconPlusOutlineRegular,
  IconProjectAddOutlineRegular, IconSearchOutlineRegular, IconSettingsOutlineRegular, IconShieldOutlineRegular, Menu,
  ShortcutKeys, Tooltip, useStore, type MenuEntry, type MenuItem,
} from '../ui/index.ts'
import { usePrefs } from '../prefs.ts'
import { ChatPane } from '../chat/ChatPane.tsx'
import { History } from '../chat/History.tsx'
import { SettingsDialog, type SettingsSection } from '../settings/SettingsDialog.tsx'
import { CtfWorkbench } from '../ctf/CtfWorkbench.tsx'
import { useCtfT } from '../ctf/messages.ts'
import type { EditorAppearance, EditorInstance } from '../ide/editor-types.ts'
import { sourceLanguage } from '../ide/ide-model.ts'
import { IdeEditor } from '../ide/IdeEditor.tsx'
import { IdeBottom } from '../ide/IdeBottom.tsx'
import { FileTree } from '../ide/FileTree.tsx'
import { QuickOpenDialog } from '../ide/QuickOpenDialog.tsx'
import { RunConfigurationDialog } from '../ide/RunConfigurationDialog.tsx'
import { fileKey, fileLabel, fileReference, keyFromAbsolute } from '../ide/ide-paths.ts'
import {
  chooseFileLanguage, inferredRunLanguage, pinRunProfile, runLanguageNames, runLanguages, runsFile, runTarget, type RunFile,
} from '../ide/run-target.ts'
import { useIdeT } from '../ide/messages.ts'
import { on } from './bus.ts'
import { useDarkTheme } from './appearance.ts'
import { DirectoryBrowser, type CreateDirectory, type ListDirectory } from './DirectoryBrowser.tsx'
import { NoFile, Welcome } from './EmptyStates.tsx'
import { clampAgent, clampBottom, clampSidebar, isNarrow, RESIZE_STEP } from './layout.ts'
import { useAppT } from './messages.ts'
import { PromptDialog, type PendingPrompt, type PromptRequest } from './PromptDialog.tsx'
import { modalOpen, QUICK_OPEN_KEYS, useGlobalShortcuts } from './shortcuts.ts'
import type { Workbench } from './workbench.ts'
import css from './App.module.css'

type MenuId = 'file' | 'edit' | 'view' | 'explorer' | 'editor' | 'app' | 'workspace' | 'run'
type Side = 'left' | 'right' | 'bottom'

const settingsSections: readonly SettingsSection[] = ['general', 'models', 'extensions', 'runtime', 'memory']

/** Inputs of the window layout. */
export interface WorkbenchLayoutProps {
  readonly workbench: Workbench
  readonly viewportWidth: number
  readonly viewportHeight: number
  /** Directory browser requests; default to the Host. */
  readonly listDirectory?: ListDirectory | undefined
  readonly createDirectory?: CreateDirectory | undefined
}

/**
 * Render the window.
 * @param props The workbench models and the viewport size.
 * @returns The complete window.
 */
export function WorkbenchLayout({ workbench, viewportWidth: width, viewportHeight: height, listDirectory, createDirectory }: WorkbenchLayoutProps) {
  const t = useIdeT()
  const tc = useCtfT()
  const ta = useAppT()
  const { model, execution, directory } = workbench
  const state = useStore(model.state)
  const executionState = useStore(execution.state)
  const directoryPending = useStore(directory.pending)
  const sessionId = useStore(workbench.session)
  const prefs = usePrefs()
  const dark = useDarkTheme()
  const appearance = useMemo<EditorAppearance>(() => ({ dark, fontSize: prefs.codeFontSize }), [dark, prefs.codeFontSize])
  const [navigation, setNavigation] = useState<'files' | 'history'>('files')
  const [selected, setSelected] = useState<IdeFileEntry | undefined>()
  const [selectedRootId, setSelectedRootId] = useState<IdeRootId | undefined>()
  const [menu, setMenu] = useState<MenuId | undefined>()
  const [prompt, setPrompt] = useState<PendingPrompt | undefined>()
  const [runConfiguration, setRunConfiguration] = useState(false)
  const [directoryOpen, setDirectoryOpen] = useState(false)
  const [directoryBusy, setDirectoryBusy] = useState(false)
  const [directoryMode, setDirectoryMode] = useState<'open' | 'attach'>('open')
  const [narrowNavigation, setNarrowNavigation] = useState(false)
  const [toolsOpen, setToolsOpen] = useState(state.center === 'tools')
  const [toolsVisited, setToolsVisited] = useState(state.center === 'tools')
  const [settings, setSettings] = useState<{ readonly open: boolean; readonly section?: SettingsSection | undefined }>({ open: false })
  useEffect(() => { if (state.center === 'tools') { setToolsOpen(true); setToolsVisited(true) } }, [state.center])
  const editor = useRef<EditorInstance | undefined>(undefined)
  const history = useRef<HTMLDivElement>(null)
  const drag = useRef<{ side: Side; origin: number; size: number } | undefined>(undefined)
  const layout = state.data.layout
  const narrow = isNarrow(width, layout.agentVisible)
  useEffect(() => { if (!narrow) setNarrowNavigation(false) }, [narrow])
  const leftVisible = layout.sidebarVisible && (!narrow || narrowNavigation)
  const leftPanelWidth = clampSidebar(layout.sidebarWidth)
  const leftWidth = leftVisible && !narrow ? leftPanelWidth : 0
  const agentSize = (value: number): number => clampAgent(value, width, leftWidth)
  const bottomSize = (value: number): number => clampBottom(value, height)
  const rightWidth = layout.agentVisible ? agentSize(layout.agentWidth) : 0
  const bottomHeight = bottomSize(layout.bottomHeight)
  const activePath = state.data.activePath
  const active = activePath === null ? undefined : state.buffers[activePath]
  const tab = state.data.tabs.find(entry => entry.path === activePath)
  const editing = state.center === 'editor' && active !== undefined
  const debug = executionState.status.debugSessions.find(entry => entry.id === executionState.debugId)
  const debugFrame = executionState.frames.find(frame => frame.id === executionState.frameId)
  const stoppedPath = debugFrame?.path !== undefined && state.workspace !== null
    ? keyFromAbsolute(state.workspace, debugFrame.path) ?? (debugFrame.path.startsWith('/') ? undefined : debugFrame.path)
    : undefined
  const environment = window.__RAINY_AGENT__
  const caret = state.selection !== undefined && state.selection.path === activePath && state.center === 'editor' ? state.selection : undefined
  const runningProgram = executionState.status.runs.find(entry => entry.phase === 'starting' || entry.phase === 'building' || entry.phase === 'running')
  const activity = debug !== undefined && debug.phase !== 'terminated' && debug.phase !== 'failed'
    ? `${t('ideDebugging')} · ${debug.name}`
    : runningProgram === undefined ? undefined : `${t('ideRunning')} · ${runningProgram.name}`
  const stopped = debug?.phase === 'paused' && stoppedPath !== undefined && debugFrame !== undefined
    ? { path: stoppedPath, line: debugFrame.line }
    : undefined

  const run = (operation: Promise<unknown>): void => {
    void operation.catch((error: unknown) => { model.fail(error) })
  }
  const openDirectory = (mode: 'open' | 'attach' = 'open'): void => {
    if (directoryPending) return
    setDirectoryMode(mode)
    if (workbench.nativeDirectory) run(directory.open(mode))
    else setDirectoryOpen(true)
  }
  const browse = (): void => {
    if (directoryPending) return
    setDirectoryMode('open')
    setDirectoryOpen(true)
  }
  const adoptDirectory = (path: string): void => {
    setDirectoryBusy(true)
    void (directoryMode === 'attach' ? model.attachRoot(path) : model.openWorkspace(path))
      .then(() => { setDirectoryOpen(false) })
      .catch((error: unknown) => { setDirectoryOpen(false); model.fail(error) })
      .finally(() => { setDirectoryBusy(false) })
  }
  const help = (): void => {
    window.dispatchEvent(new CustomEvent('rainy:native-menu', { detail: { menu: 'help', locale: prefs.locale } }))
  }
  const request = (next: PromptRequest): Promise<string | null> =>
    new Promise((resolve) => { setPrompt({ ...next, resolve }) })
  const resolvePrompt = (value: string | null): void => {
    prompt?.resolve(value)
    setPrompt(undefined)
  }
  useEffect(() => () => { prompt?.resolve(null) }, [prompt])
  useEffect(() => {
    setSelected(undefined)
    setSelectedRootId(undefined)
  }, [state.workspace?.workspaceId])

  const closeFile = async (path: string): Promise<void> => {
    if (state.buffers[path]?.dirty) {
      const result = await request({ title: t('ideDirtyTitle'), description: `${fileLabel(state.workspace, path)}\n${t('ideDirtyDescription')}`, dirty: true })
      if (result === null || (result === 'save' && (!(await model.save(path)) || model.state.getSnapshot().buffers[path]?.dirty))) return
    }
    model.close(path)
  }
  const reloadFile = async (): Promise<void> => {
    if (activePath === null) return
    if (active?.dirty && (await request({ title: t('ideDiscard'), description: fileLabel(state.workspace, activePath) })) === null) return
    await model.reload(activePath)
  }
  const create = async (folder: boolean): Promise<void> => {
    const reference = selected === undefined ? { path: '', rootId: selectedRootId } : fileReference(selected.path)
    const base = selected === undefined
      ? ''
      : selected.kind === 'directory' || selected.targetKind === 'directory'
        ? reference.path + '/'
        : reference.path.slice(0, reference.path.lastIndexOf('/') + 1)
    const path = await request({ title: t(folder ? 'ideNewFolder' : 'ideNewFile'), initial: base })
    if (path !== null && path.trim() !== '') await model.create(fileKey(path.trim(), reference.rootId), folder)
  }
  const rename = async (): Promise<void> => {
    if (selected === undefined) return
    const reference = fileReference(selected.path)
    const path = await request({ title: t('ideRename'), initial: reference.path })
    if (path !== null && path.trim() !== '' && path !== reference.path) await model.rename(selected, fileKey(path.trim(), reference.rootId))
  }
  const remove = async (): Promise<void> => {
    if (selected === undefined) return
    await model.remove(selected.path, async (entries, bytes) =>
      (await request({ title: t('ideDelete'), description: t('ideDeleteDescription', { path: fileLabel(state.workspace, selected.path), entries, bytes }) })) !== null)
  }
  const renameWorkspace = async (): Promise<void> => {
    const workspace = state.workspace
    if (workspace === null) return
    const title = await request({ title: t('ideRenameWorkspace'), initial: workspace.title, label: t('ideProjectName') })
    if (title !== null && title.trim() !== '' && title.trim() !== workspace.title) await model.renameWorkspace(workspace.workspaceId, title.trim())
  }
  const removeWorkspace = async (): Promise<void> => {
    const workspace = state.workspace
    if (workspace === null) return
    if ((await request({ title: t('ideRemoveWorkspace'), description: t('ideRemoveWorkspaceDescription', { title: workspace.title }) })) === null) return
    await model.removeWorkspace(workspace.workspaceId)
  }

  const activeReference = activePath === null ? undefined : fileReference(activePath)
  const runFile: RunFile | undefined = activePath === null || activeReference === undefined ? undefined
    : { path: activePath, label: fileLabel(state.workspace, activePath), program: activeReference.path,
      ...activeReference.rootId === undefined ? {} : { rootId: activeReference.rootId } }
  const target = runTarget(state.data.execution, runFile)
  const configuration = (): IdeRunConfiguration | undefined => target?.configuration
  const inferred = activePath === null ? undefined : inferredRunLanguage(activePath)
  const fileLanguage = target !== undefined && target.mode !== 'pinned' ? target.configuration.language : undefined
  const checked = (value: boolean): ReactNode => value ? <IconCheckOutlineRegular size={14} /> : <span className={css.menuCheck} />
  const otherProfiles = (state.data.execution?.profiles ?? []).filter(profile => runFile === undefined || !runsFile(profile, runFile)).slice(0, 8)
  const runMenu: MenuEntry[] = [
    { type: 'label', id: 'current-file', text: t('ideRunCurrentFile') },
    { id: 'auto', label: inferred === undefined ? t('ideRunAutoUnknown') : t('ideRunAuto', { language: runLanguageNames[inferred] }),
      disabled: inferred === undefined, icon: checked(fileLanguage !== undefined && fileLanguage === inferred) },
    ...runLanguages.map(language => ({ id: `language:${language}`, label: runLanguageNames[language], disabled: runFile === undefined,
      icon: checked(fileLanguage === language && language !== inferred) })),
    ...otherProfiles.length === 0 ? [] : [{ type: 'separator' as const, id: 'pinned-separator' }, { type: 'label' as const, id: 'pinned', text: t('ideRunPinned') },
      ...otherProfiles.map(profile => ({ id: `profile:${profile.name}`, label: profile.name,
        icon: checked(target?.mode === 'pinned' && target.configuration.name === profile.name) }))],
    { type: 'separator', id: 'edit-separator' },
    { id: 'edit', label: t('ideRunEdit') },
  ]
  const chooseRun = (id: string): void => {
    setMenu(undefined)
    const current = state.data.execution
    if (id === 'edit') setRunConfiguration(true)
    else if (id.startsWith('profile:')) model.execution(pinRunProfile(current, id.slice('profile:'.length)))
    else if (runFile !== undefined) {
      const choice = id === 'auto' ? 'auto' : runLanguages.find(language => id === `language:${language}`)
      if (choice !== undefined) model.execution(chooseFileLanguage(current, runFile, choice))
    }
  }
  const runLabel = target === undefined ? t('ideRun') : t('ideRunWith', {
    target: target.mode === 'pinned' ? target.configuration.name : runLanguageNames[target.configuration.language] })
  const launch = async (debugging: boolean): Promise<void> => {
    const config = configuration()
    if (config === undefined) { setRunConfiguration(true); return }
    if (!(await model.saveAll())) return
    model.layout({ bottomVisible: true, bottomTab: debugging ? 'debug' : 'terminal' })
    if (debugging) await execution.debug(config)
    else await execution.run(config)
  }
  const showLeft = (mode: 'files' | 'history'): void => {
    setNavigation(mode)
    setNarrowNavigation(narrow)
    model.layout({ sidebarVisible: true })
  }
  const showHistory = (): void => {
    showLeft('history')
    model.layout({ agentVisible: true })
  }
  const newChat = (): void => {
    workbench.showSession(null)
    model.layout({ agentVisible: true })
  }
  const openSettings = (section: SettingsSection): void => { setSettings({ open: true, section }) }
  const newTerminal = (): void => {
    if (state.workspace === null) return
    model.layout({ bottomVisible: true, bottomTab: 'terminal' })
    run(execution.terminal())
  }
  const action = (id: string): void => {
    setMenu(undefined)
    const actions: Record<string, () => void> = {
      folder: () => { openDirectory('open') },
      wsl: browse,
      quickOpen: () => { model.quickOpen(true) },
      profiles: () => { setRunConfiguration(true) },
      file: () => { run(create(false)) },
      directory: () => { run(create(true)) },
      save: () => { run(model.save()) },
      saveAll: () => { run(model.saveAll()) },
      rename: () => { run(rename()) },
      delete: () => { run(remove()) },
      refresh: () => { run(model.refreshTree()) },
      openFolder: () => { openDirectory('open') },
      attachFolder: () => { openDirectory('attach') },
      help,
      history: showHistory,
      compare: () => {
        if (activePath === null) return
        if (tab?.kind === 'diff') model.edit(activePath)
        else run(model.diff(activePath))
      },
      selection: () => { workbench.sendSelection() },
      focus: () => { model.layout({ agentVisible: false, bottomVisible: false }); model.center('editor') },
      format: () => { run(model.format()) },
      find: () => { run(editor.current?.action('actions.find') ?? Promise.resolve()) },
      replace: () => { run(editor.current?.action('editor.action.startFindReplaceAction') ?? Promise.resolve()) },
      line: () => { run(editor.current?.action('editor.action.gotoLine') ?? Promise.resolve()) },
      files: () => {
        if (narrow) {
          setNarrowNavigation(!leftVisible)
          model.layout({ sidebarVisible: true })
        } else model.layout({ sidebarVisible: !layout.sidebarVisible })
        setNavigation('files')
      },
      agent: () => { model.layout({ agentVisible: !layout.agentVisible }) },
      bottom: () => { model.layout({ bottomVisible: !layout.bottomVisible }) },
      tools: () => { model.center('tools') },
    }
    actions[id]?.()
  }

  const shortcuts = {
    quickOpen: () => { if (state.workspace !== null) model.quickOpen(true) },
    settings: () => {
      if (settings.open) setSettings({ open: false })
      else if (!modalOpen()) openSettings('general')
    },
    newChat,
    searchHistory: () => {
      showLeft('history')
      requestAnimationFrame(() => { history.current?.querySelector<HTMLElement>('input, textarea, [role="searchbox"]')?.focus() })
    },
    openFolder: () => { openDirectory('open') },
    newTerminal,
  }
  useGlobalShortcuts(shortcuts)
  const showPane = useRef<(pane: string, section: string | undefined) => void>(() => undefined)
  showPane.current = (pane, section) => {
    if (pane === 'ai') model.layout({ agentVisible: true })
    else if (pane === 'files' || pane === 'history') showLeft(pane)
    else if (pane === 'bottom') model.layout({ bottomVisible: true })
    else if (pane === 'ctf') model.center('tools')
    else if (pane === 'settings') openSettings(settingsSections.find(entry => entry === section) ?? 'general')
  }
  useEffect(() => on('pane.show', ({ pane, section }) => { showPane.current(pane, section) }), [])

  const beginDrag = (event: PointerEvent<HTMLDivElement>, side: Side): void => {
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = { side, origin: side === 'bottom' ? event.clientY : event.clientX,
      size: side === 'left' ? leftWidth : side === 'right' ? rightWidth : bottomHeight }
  }
  const moveDrag = (event: PointerEvent<HTMLDivElement>): void => {
    const current = drag.current
    if (current === undefined) return
    const delta = (current.side === 'bottom' ? event.clientY : event.clientX) - current.origin
    if (current.side === 'left') model.layout({ sidebarWidth: clampSidebar(current.size + delta) })
    else if (current.side === 'right') model.layout({ agentWidth: agentSize(current.size - delta) })
    else model.layout({ bottomHeight: bottomSize(current.size - delta) })
  }
  const resize = (side: Side, position?: number): ReactNode => (
    <div
      role="separator"
      tabIndex={0}
      aria-orientation={side === 'bottom' ? 'horizontal' : 'vertical'}
      aria-label={t(side === 'left' ? 'ideResizeFiles' : side === 'right' ? 'ideResizeAgent' : 'ideResizeBottom')}
      className={side === 'bottom' ? css.resizeBottom : css.resize}
      style={position === undefined ? undefined : { left: position - 2 }}
      onPointerDown={(event) => { beginDrag(event, side) }}
      onPointerMove={moveDrag}
      onPointerUp={() => { drag.current = undefined }}
      onLostPointerCapture={() => { drag.current = undefined }}
      onKeyDown={(event) => {
        const delta = event.key === 'ArrowLeft' || event.key === 'ArrowUp' ? -RESIZE_STEP
          : event.key === 'ArrowRight' || event.key === 'ArrowDown' ? RESIZE_STEP : 0
        if (delta === 0) return
        event.preventDefault()
        if (side === 'left') model.layout({ sidebarWidth: clampSidebar(leftWidth + delta) })
        else if (side === 'right') model.layout({ agentWidth: agentSize(rightWidth - delta) })
        else model.layout({ bottomHeight: bottomSize(bottomHeight - delta) })
      }}
    />
  )
  const menuItems = (kind: 'file' | 'edit' | 'view'): MenuItem[] =>
    kind === 'file'
      ? [
        { id: 'folder', label: t('ideWindowsFolder'), disabled: !workbench.nativeDirectory || directoryPending },
        { id: 'wsl', label: t('ideWslFolder'), disabled: directoryPending },
        { id: 'quickOpen', label: t('ideQuickOpen'), disabled: state.workspace === null },
        { id: 'file', label: t('ideNewFile'), disabled: state.workspace === null },
        { id: 'directory', label: t('ideNewFolder'), disabled: state.workspace === null },
        { id: 'save', label: t('ideSave'), disabled: activePath === null || active?.source === 'snippet' },
        { id: 'saveAll', label: t('ideSaveAll'), disabled: activePath === null },
        { id: 'profiles', label: t('ideRunProfiles'), disabled: state.workspace === null },
      ]
      : kind === 'edit'
        ? [
          { id: 'find', label: t('ideFind') },
          { id: 'replace', label: t('ideReplace') },
          { id: 'line', label: t('ideGoLine') },
          { id: 'format', label: t('ideFormat') },
        ]
        : [
          { id: 'focus', label: t('ideFocusEditor') },
          { id: 'files', label: t('ideToggleFiles') },
          { id: 'agent', label: t('ideToggleAgent') },
          { id: 'bottom', label: t('ideToggleBottom') },
          { id: 'tools', label: t('ideTools') },
        ]
  const workspaceMenu: MenuEntry[] = [
    ...state.workspaces.map(workspace => ({ id: workspace.workspaceId, label: workspace.title })),
    { id: 'rainy:open-folder', label: t('ideOpenFolder') },
    ...state.workspace === null ? [] : [
      { type: 'separator' as const, id: 'rainy:current' },
      { id: 'rainy:rename-workspace', label: t('ideRenameWorkspace') },
      { id: 'rainy:remove-workspace', label: t('ideRemoveWorkspace'), danger: true },
    ],
  ]
  const chooseWorkspace = (id: string): void => {
    setMenu(undefined)
    if (id === 'rainy:open-folder') openDirectory()
    else if (id === 'rainy:rename-workspace') run(renameWorkspace())
    else if (id === 'rainy:remove-workspace') run(removeWorkspace())
    else {
      const workspace = state.workspaces.find(item => item.workspaceId === id)
      if (workspace !== undefined) run(model.selectWorkspace(workspace))
    }
  }
  const toggle = (id: MenuId): void => { setMenu(menu === id ? undefined : id) }

  return (
    <div className={css.app} data-app-frame>
      <header className={css.topbar} data-window-drag data-rainy-topbar aria-label={ta('appMenu')}>
        <div className={css.topStart}>
          <img src="/rainy/icon.png" alt="" className={css.topMark} />
          {width < 900 ? <Menu open={menu === 'app'} onClose={() => { setMenu(undefined) }} portal compact
            anchor={<IconAction label={ta('appMenu')} expanded={menu === 'app'} onClick={() => { toggle('app') }}>
              <IconEllipsisOutlineRegular size={16} />
            </IconAction>}
            items={[
              { id: 'file-menu', label: ta('fileMenu'), submenu: menuItems('file') },
              { id: 'edit-menu', label: ta('editMenu'), submenu: menuItems('edit') },
              { id: 'view-menu', label: ta('viewMenu'), submenu: menuItems('view') },
              { id: 'help', label: ta('helpMenu') },
            ]} onSelect={action} /> : <>
            {(['file', 'edit', 'view'] as const).map(kind => (
              <Menu key={kind} open={menu === kind} onClose={() => { setMenu(undefined) }} portal
                anchor={<button type="button" className={css.button} aria-haspopup="menu" aria-expanded={menu === kind} onClick={() => { toggle(kind) }}>
                  {ta(kind === 'file' ? 'fileMenu' : kind === 'edit' ? 'editMenu' : 'viewMenu')}
                </button>}
                items={menuItems(kind)} onSelect={action} />
            ))}
            <button type="button" className={css.button} onClick={help}>{ta('helpMenu')}</button>
          </>}
        </div>
        <div className={css.commandCenter}>
          <Menu open={menu === 'workspace'} onClose={() => { setMenu(undefined) }} portal compact className={css.commandMenu}
            anchor={<button type="button" className={css.commandWorkspace} aria-label={t('ideWorkspace')} title={state.workspace?.path}
              aria-haspopup="menu" aria-expanded={menu === 'workspace'} disabled={state.phase === 'loading'}
              onClick={() => { toggle('workspace') }}>
              <IconFolderCloseRegular size={14} />
              <span>{state.workspace?.title ?? t('ideOpenFolder')}</span>
              <IconChevronDownOutlineRegular size={12} />
            </button>}
            items={workspaceMenu} onSelect={chooseWorkspace} />
          <Tooltip label={t('ideQuickOpen')} shortcutKeys={QUICK_OPEN_KEYS} side="bottom" portal>
            <button type="button" className={css.commandSearch} aria-label={t('ideQuickOpen')} disabled={state.workspace === null}
              onClick={() => { model.quickOpen(true) }}>
              <IconSearchOutlineRegular size={14} />
              <span className={css.commandLabel}>{t('ideSearchFiles')}</span>
              <ShortcutKeys keys={QUICK_OPEN_KEYS} className={css.shortcut} />
            </button>
          </Tooltip>
        </div>
        <div className={css.topEnd}>
          {editing && <div className={css.toolGroup}>
            <IconAction label={runLabel} onClick={() => { run(launch(false)) }}><IconPlayOutlineRegular size={16} /></IconAction>
            {target?.configuration.language !== 'php'
              && <IconAction label={t('ideDebug')} onClick={() => { run(launch(true)) }}><IconBugOutline size={16} /></IconAction>}
            <Menu open={menu === 'run'} onClose={() => { setMenu(undefined) }} portal align="end" compact items={runMenu} onSelect={chooseRun}
              anchor={<IconAction label={t('ideRunMode')} expanded={menu === 'run'} onClick={() => { toggle('run') }}>
                <IconChevronDownOutlineRegular size={12} />
              </IconAction>} />
          </div>}
          <div className={css.toolGroup}>
            <IconAction label={t('ideToggleFiles')} pressed={leftVisible} onClick={() => { action('files') }}>
              <IconPanelLeftOutlineRegular size={16} />
            </IconAction>
            <IconAction label={t('ideToggleBottom')} pressed={layout.bottomVisible} onClick={() => { action('bottom') }}>
              <IconPanelBottomOutline size={16} />
            </IconAction>
            <IconAction label={t('ideToggleAgent')} pressed={layout.agentVisible} onClick={() => { action('agent') }}>
              <IconPanelRightOutline size={16} />
            </IconAction>
          </div>
          <div className={css.toolGroup}>
            <IconAction label={tc('ctf')} pressed={state.center === 'tools'} onClick={() => { model.center('tools') }}>
              <IconShieldOutlineRegular size={16} />
            </IconAction>
            <IconAction label={ta('settings')} onClick={() => { openSettings('models') }}>
              <IconSettingsOutlineRegular size={16} />
            </IconAction>
          </div>
        </div>
      </header>
      <div className={css.root} data-rainy-ide style={{ gridTemplateColumns: `${leftWidth}px minmax(0, 1fr) ${rightWidth}px` }}>
        <aside className={`${css.pane} ${css.left}${narrow && leftVisible ? ` ${css.overlay}` : ''}`} hidden={!leftVisible}
          aria-label={t('ideWorkspace')} style={narrow && leftVisible ? { position: 'absolute', width: leftPanelWidth } : undefined}>
          <div className={css.explorerHeader}>
            {navigation === 'history' && <IconAction label={t('ideBackToFiles')} onClick={() => { setNavigation('files') }}>
              <IconChevronLeftOutlineRegular size={14} />
            </IconAction>}
            <span className={css.paneTitle} title={state.workspace?.path}>
              {navigation === 'history' ? t('ideHistory') : (state.workspace?.title ?? t('ideExplorer'))}
            </span>
            {navigation === 'files' && <div className={css.explorerActions}>
              <IconAction label={t('ideAddFolder')} disabled={state.phase === 'loading' || directoryPending} onClick={() => { openDirectory('attach') }}>
                <IconProjectAddOutlineRegular size={14} />
              </IconAction>
              <IconAction label={t('ideNewFile')} disabled={state.workspace === null} onClick={() => { run(create(false)) }}>
                <IconPlusOutlineRegular size={14} />
              </IconAction>
              <IconAction label={t('ideNewFolder')} disabled={state.workspace === null} onClick={() => { run(create(true)) }}>
                <IconFolderCloseRegular size={14} />
              </IconAction>
              <Menu open={menu === 'explorer'} onClose={() => { setMenu(undefined) }} portal align="end" compact
                anchor={<IconAction label={t('ideFileActions')} expanded={menu === 'explorer'} onClick={() => { toggle('explorer') }}>
                  <IconEllipsisOutlineRegular size={14} />
                </IconAction>}
                items={[
                  { id: 'openFolder', label: t('ideOpenFolder'), disabled: directoryPending },
                  { id: 'attachFolder', label: t('ideAddFolder'), disabled: directoryPending },
                  { id: 'rename', label: t('ideRename'), disabled: selected === undefined },
                  { id: 'delete', label: t('ideDelete'), disabled: selected === undefined, danger: true },
                  { id: 'refresh', label: t('ideRefresh'), disabled: state.workspace === null },
                  { id: 'history', label: t('ideHistory') },
                ]} onSelect={action} />
            </div>}
            {narrow && <IconAction label={t('ideClose')} onClick={() => { setNarrowNavigation(false) }}>
              <IconCloseOutlineRegular size={14} />
            </IconAction>}
          </div>
          <div className={css.retained} hidden={navigation !== 'files'}>
            <FileTree state={state} selected={selected?.path} selectedRootId={selectedRootId}
              select={(entry, rootId) => { setSelected(entry); setSelectedRootId(rootId) }}
              selectRoot={(rootId) => { setSelected(undefined); setSelectedRootId(rootId) }}
              activate={(entry) => {
                const folder = entry.kind === 'directory' || entry.targetKind === 'directory'
                run(folder ? model.toggleDirectory(entry.path) : model.openFile(entry.path))
                if (!folder) setNarrowNavigation(false)
              }}
              removeRoot={(rootId) => {
                run(model.removeRoot(rootId).then(() => {
                  if (selectedRootId === rootId) { setSelected(undefined); setSelectedRootId(undefined) }
                }))
              }} />
          </div>
          <div className={css.retained} hidden={navigation !== 'history'} ref={history}>
            <History currentSessionId={sessionId} onNewChat={newChat}
              onSelect={(summary) => { setNarrowNavigation(false); void workbench.openSession(summary) }} />
          </div>
        </aside>
        <main className={`${css.pane} ${css.center}`}>
          <div className={css.editorArea}>
            <div className={css.editorHeader} hidden={state.data.tabs.length === 0 && !toolsOpen}>
              <div className={css.tabs} role="tablist" aria-label={t('ideExplorer')}>
                {state.data.tabs.map(file => (
                  <div className={css.tab} key={file.path} data-active={(activePath === file.path && state.center === 'editor') || undefined}>
                    <button type="button" role="tab" className={css.button}
                      aria-selected={activePath === file.path && state.center === 'editor'} title={fileLabel(state.workspace, file.path)}
                      onClick={() => { run(model.openFile(file.path)) }}>
                      <FileTypeIcon path={file.path} size={14} />
                      <span className={css.tabName}>{file.path.split('/').at(-1)}</span>
                      {state.buffers[file.path]?.dirty && <span className={css.dirty} aria-hidden="true">●</span>}
                    </button>
                    <IconAction label={`${t('ideClose')} ${fileLabel(state.workspace, file.path)}`} onClick={() => { run(closeFile(file.path)) }}>
                      <IconCloseOutlineRegular size={12} />
                    </IconAction>
                  </div>
                ))}
                {toolsOpen && <div className={css.tab} data-active={state.center === 'tools' || undefined}>
                  <button type="button" role="tab" aria-selected={state.center === 'tools'} className={css.button}
                    onClick={() => { model.center('tools') }}>{tc('ctf')}</button>
                  <IconAction label={tc('ctfClose')} onClick={() => { setToolsOpen(false); model.center('editor') }}>
                    <IconCloseOutlineRegular size={12} />
                  </IconAction>
                </div>}
              </div>
              {editing && <div className={css.editorActions}>
                {active.dirty && <Button size="sm" onClick={() => { run(model.save()) }}>{t('ideSave')}</Button>}
                <Menu open={menu === 'editor'} onClose={() => { setMenu(undefined) }} portal align="end" compact
                  anchor={<IconAction label={t('ideEditorActions')} expanded={menu === 'editor'} onClick={() => { toggle('editor') }}>
                    <IconEllipsisOutlineRegular size={16} />
                  </IconAction>}
                  items={[
                    { id: 'save', label: t('ideSave'), disabled: !active.dirty },
                    { id: 'format', label: t('ideFormat'), disabled: active.source === 'snippet' },
                    { id: 'compare', label: t(tab?.kind === 'diff' ? 'ideEditFile' : 'ideCompare'), disabled: active.source === 'snippet' && tab?.kind !== 'diff' },
                    { id: 'selection', label: t('ideSendSelection'), disabled: state.selection === undefined },
                  ]} onSelect={action} />
              </div>}
            </div>
            {state.error && <div className={`${css.notice} ${css.error}`} role="alert">{state.error}</div>}
            {active?.comparison?.kind === 'snippet' && activePath !== null && (
              <div className={css.notice}>
                <span>{t('ideCompareSnippet')}: {fileLabel(state.workspace, active.comparison.target ?? '')}</span>
                <button type="button" className={css.button} onClick={() => {
                  run(model.applySnippet(activePath).then((result) => {
                    if (result !== 'applied') model.fail(new Error(t(result === 'changed' ? 'ideSnippetChanged' : 'ideSnippetMissing')))
                  }))
                }}>{t('ideApplySnippet')}</button>
              </div>
            )}
            {state.recoveryConflict && <div className={`${css.notice} ${css.error}`} role="alert">{t('ideRecoveryConflict')}</div>}
            {active?.external && (
              <div className={css.notice} role="status">
                <span>{t('ideExternal')}</span>
                <button type="button" className={css.button} onClick={() => { run(reloadFile()) }}>{t('ideReload')}</button>
                {active.comparison?.kind === 'conflict' && (
                  <button type="button" className={css.button} onClick={() => { if (activePath !== null) model.acceptConflict(activePath) }}>
                    {t('ideReviewOverwrite')}
                  </button>
                )}
              </div>
            )}
            <div className={css.centerBody} hidden={state.center !== 'editor'}>
              <IdeEditor
                model={model}
                state={state}
                appearance={appearance}
                sendSelection={() => { workbench.sendSelection() }}
                attach={(value) => { editor.current = value }}
                breakpoint={(path, line) => { run(execution.toggleBreakpoint(path, line)) }}
                stopped={stopped}
                empty={state.workspace === null
                  ? <Welcome workspaces={state.workspaces} loading={state.phase === 'loading'} nativeDirectory={workbench.nativeDirectory}
                    pending={directoryPending} open={() => { openDirectory('open') }} browse={browse}
                    select={(workspace) => { run(model.selectWorkspace(workspace)) }} />
                  : <NoFile shortcut={QUICK_OPEN_KEYS} search={() => { model.quickOpen(true) }} create={() => { run(create(false)) }}
                    agent={layout.agentVisible ? undefined : () => { model.layout({ agentVisible: true }) }} />}
              />
            </div>
            <div className={css.centerBody} hidden={state.center !== 'tools'}>
              {toolsVisited && <CtfWorkbench owner={workbench.ctf} sessionId={sessionId} active={state.center === 'tools'} dark={dark} />}
            </div>
          </div>
          <div className={css.bottom} hidden={!layout.bottomVisible}
            style={{ height: layout.bottomVisible ? bottomHeight : 0, minHeight: layout.bottomVisible ? undefined : 0 }}>
            {resize('bottom')}
            <IdeBottom state={state} executionState={executionState} execution={execution} model={model} appearance={appearance}
              reveal={(path, line, column) => { run(model.reveal(path, line, column)) }} />
          </div>
        </main>
        <aside className={`${css.pane} ${css.agent}`} hidden={!layout.agentVisible} aria-label={t('ideAgent')}>
          <div className={css.retained} data-rainy-agent>
            <ChatPane workspace={state.workspace} sessionId={sessionId}
              onSessionChange={(next) => { workbench.showSession(next) }}
              onClose={() => { model.layout({ agentVisible: false }) }}
              onShowHistory={showHistory} />
          </div>
        </aside>
        <footer className={css.statusBar} data-rainy-status>
          {environment !== undefined && <span className={css.statusItem} title={`${environment.name} ${environment.version}`}>
            <span className={css.statusDot} aria-hidden="true" />{environment.environment}
          </span>}
          {state.saving && <span className={css.statusItem}>{t('ideRecoverySaving')}</span>}
          {activity !== undefined && <span className={css.statusItem} data-activity>{activity}</span>}
          <span className={css.spacer} />
          {caret !== undefined && <span className={css.statusItem}>{t('ideStatusPosition', { line: caret.endLine, column: caret.endColumn })}</span>}
          {caret !== undefined && caret.text.length > 0 && <span className={css.statusItem}>{t('ideStatusSelected', { count: caret.text.length })}</span>}
          {active !== undefined && <>
            <span className={css.statusItem}>{active.language ?? sourceLanguage(active.document.path)}</span>
            <span className={css.statusItem}>{active.document.eol.toUpperCase()}</span>
            <span className={css.statusItem}>{t('ideEncoding')}{active.document.bom ? ` ${t('ideBom')}` : ''}</span>
          </>}
        </footer>
        {leftVisible && !narrow && resize('left', leftWidth)}
        {layout.agentVisible && resize('right', width - rightWidth)}
      </div>
      <PromptDialog prompt={prompt} close={resolvePrompt} />
      <QuickOpenDialog model={model} state={state} />
      <DirectoryBrowser open={directoryOpen} busy={directoryBusy} onOpen={adoptDirectory} onClose={() => { setDirectoryOpen(false) }}
        listDirectory={listDirectory} createDirectory={createDirectory} />
      <RunConfigurationDialog open={runConfiguration} close={() => { setRunConfiguration(false) }} state={state} model={model} />
      <SettingsDialog open={settings.open} section={settings.section} workspace={state.workspace} onClose={() => { setSettings({ open: false }) }} />
    </div>
  )
}
