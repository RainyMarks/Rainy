/** The window: top bar, side pane, editor area with the project map, AI pane, bottom panel and status bar. */
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { PointerEvent, ReactNode } from 'react'
import type { IdeFileEntry, IdeRootId } from '../../shared/ide-files-protocol.ts'
import type { IdeRunConfiguration } from '../../shared/ide-execution-protocol.ts'
import {
  Button, FileTypeIcon, fileSizeText, IconAction, IconBugOutline, IconCheckOutlineRegular, IconChevronDownOutlineRegular,
  IconChevronLeftOutlineRegular, IconCloseOutlineRegular, IconEllipsisOutlineRegular, IconFolderCloseRegular,
  IconPanelBottomOutline, IconPanelLeftOutlineRegular, IconPanelRightOutline, IconPlayOutlineRegular, IconPlusOutlineRegular,
  IconMapOutline, IconProjectAddOutlineRegular, IconSearchOutlineRegular, IconSettingsOutlineRegular, Menu, RainyMark,
  IconBranchOutlineRegular, IconFilesOutline, IconSourceControlOutline, ShortcutKeys, Tooltip, useStore, type MenuEntry, type MenuItem,
} from '../ui/index.ts'
import { GitView } from '../git/GitView.tsx'
import { refreshGit, useGitStatus } from '../git/git-store.ts'
import { useGitT } from '../git/messages.ts'
import { SearchView, type SearchFocus } from '../search/SearchView.tsx'
import { useSearchT } from '../search/messages.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { useAiActivity } from '../map/activity.ts'
import { useMapT } from '../map/messages.ts'
import { aiMarks, markedDirectories } from '../map/marks.ts'
import { useChanges } from '../review/changes-store.ts'
import { finishReview, openReview, undoReview } from '../review/review-actions.ts'
import { ReviewBar } from '../review/ReviewBar.tsx'
import { ChatPane } from '../chat/ChatPane.tsx'
import { useChatBusy } from '../chat/chat-store.ts'
import { History } from '../chat/History.tsx'
import { SettingsDialog, type SettingsSection } from '../settings/SettingsDialog.tsx'
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
import { useViewerT } from '../ide/viewers/messages.ts'
import { defaultView, switchableViews } from '../ide/viewers/viewer-choice.ts'
import { emit, on } from './bus.ts'
import { useDarkTheme } from './appearance.ts'
import { DirectoryBrowser, type CreateDirectory, type ListDirectory } from './DirectoryBrowser.tsx'
import { NoFile, Welcome } from './EmptyStates.tsx'
import { clampAgent, clampBottom, clampSidebar, isNarrow, preferredAgentWidth, RESIZE_STEP } from './layout.ts'
import { useAppT } from './messages.ts'
import { PromptDialog, type PendingPrompt, type PromptRequest } from './PromptDialog.tsx'
import { CommandPalette } from './CommandPalette.tsx'
import type { PaletteCommand } from './commands.ts'
import { modalOpen, PALETTE_KEYS, QUICK_OPEN_KEYS, useGlobalShortcuts } from './shortcuts.ts'
import type { Workbench } from './workbench.ts'
import css from './App.module.css'

/** The project map loads the first time it is opened. */
const ProjectMapView = lazy(async () => ({ default: (await import('../map/ProjectMap.tsx')).ProjectMapView }))

type MenuId = 'file' | 'edit' | 'view' | 'explorer' | 'editor' | 'app' | 'workspace' | 'run'
type Side = 'left' | 'right' | 'bottom'

const settingsSections: readonly SettingsSection[] = ['general', 'models', 'extensions', 'runtime', 'memory', 'security']

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
  const tv = useViewerT()
  const tm = useMapT()
  const ta = useAppT()
  const ts = useSearchT()
  const tg = useGitT()
  const { model, execution, directory } = workbench
  const state = useStore(model.state)
  const executionState = useStore(execution.state)
  const directoryPending = useStore(directory.pending)
  const sessionId = useStore(workbench.session)
  // Stable, so the chat transcript's memoized turns do not render again with every layout change.
  const showSession = useCallback((next: string | null) => { workbench.showSession(next) }, [workbench])
  const agentBusy = useChatBusy(sessionId)
  const prefs = usePrefs()
  const dark = useDarkTheme()
  const appearance = useMemo<EditorAppearance>(() => {
    // `prefs.ts` has applied the color scheme and code font named in the dependencies by the time this runs.
    const style = getComputedStyle(document.body)
    const value = (name: string): string => style.getPropertyValue(name).trim()
    const [fontFamily, background, foreground] = [value('--rainy-font-family-code'), value('--rainy-alias-bg-base'), value('--rainy-alias-label-primary')]
    return { dark, fontSize: prefs.codeFontSize, ...fontFamily === '' ? {} : { fontFamily },
      ...background === '' || foreground === '' ? {} : { background, foreground } }
  }, [dark, prefs.codeFontSize, prefs.codeFont, prefs.colorScheme])
  const [navigation, setNavigation] = useState<'files' | 'search' | 'git' | 'history'>('files')
  const [searchFocus, setSearchFocus] = useState<SearchFocus | undefined>()
  const [palette, setPalette] = useState<string | undefined>()
  const [selected, setSelected] = useState<IdeFileEntry | undefined>()
  const [selectedRootId, setSelectedRootId] = useState<IdeRootId | undefined>()
  const [menu, setMenu] = useState<MenuId | undefined>()
  const [prompt, setPrompt] = useState<PendingPrompt | undefined>()
  const [runConfiguration, setRunConfiguration] = useState(false)
  const [directoryOpen, setDirectoryOpen] = useState(false)
  const [directoryBusy, setDirectoryBusy] = useState(false)
  const [directoryMode, setDirectoryMode] = useState<'open' | 'attach'>('open')
  const [narrowNavigation, setNarrowNavigation] = useState(false)
  const [settings, setSettings] = useState<{ readonly open: boolean; readonly section?: SettingsSection | undefined }>({ open: false })
  const [mapOpen, setMapOpen] = useState(state.center === 'map')
  useEffect(() => { if (state.center === 'map') setMapOpen(true) }, [state.center])
  const editor = useRef<EditorInstance | undefined>(undefined)
  const [reviewBusy, setReviewBusy] = useState(false)
  useEffect(() => on('review.open', ({ sessionId: chat, path, toolCallIds }) => {
    void openReview(model, chat, path, toolCallIds).catch((error: unknown) => { model.fail(error) })
  }), [model])
  /** Send a message to the AI in the current chat, showing the AI pane. */
  const askAi = (text: string): void => {
    const workspace = model.state.getSnapshot().workspace
    if (workspace === null) return
    model.layout({ agentVisible: true })
    emit('chat.send', { workspaceId: workspace.workspaceId, text })
  }
  const reviewStep = (operation: () => Promise<unknown>): void => {
    setReviewBusy(true)
    void operation().catch((error: unknown) => { model.fail(error) }).finally(() => { setReviewBusy(false) })
  }
  const changes = useChanges(sessionId)
  const gitStatus = useGitStatus(state.workspace?.workspaceId ?? null).status
  const gitChanges = gitStatus?.repositories.reduce((sum, repository) => sum + repository.files.length, 0) ?? 0
  const gitBranch = gitStatus?.repositories[0]
  const aiActivity = useAiActivity()
  const treeMarks = useMemo(() => {
    const files = aiMarks(state.workspace, changes, aiActivity.active)
    return { files, directories: markedDirectories(files) }
  }, [state.workspace, changes, aiActivity.active])
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
  const rightWidth = layout.agentVisible ? agentSize(preferredAgentWidth(layout.agentWidth, width)) : 0
  const bottomHeight = bottomSize(layout.bottomHeight)
  const activePath = state.data.activePath
  const active = activePath === null ? undefined : state.buffers[activePath]
  const tab = state.data.tabs.find(entry => entry.path === activePath)
  const editing = state.center === 'editor' && active !== undefined
  const reviewing = editing && active.comparison?.kind === 'ai' ? active.comparison.review : undefined
  const views = activePath === null || active === undefined ? [] : switchableViews(activePath, active.document.readOnlyReason)
  const currentView = tab?.view ?? (activePath === null ? 'source' : defaultView(activePath))
  /** Store a view, leaving the tab without an explicit choice when it picks its type's default. */
  const showView = (view: 'source' | 'preview' | 'split' | 'hex' | 'rich'): boolean => {
    if (activePath === null || !editing) return false
    model.setView(activePath, view === defaultView(activePath) ? undefined : view)
    return true
  }
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
  const showLeft = (mode: 'files' | 'search' | 'git' | 'history'): void => {
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
  /** Show the search pane, filled with the selected text when it is a single line. */
  const openSearch = (replace: boolean): void => {
    if (state.workspace === null) return
    const text = state.selection?.text ?? ''
    setSearchFocus({ at: Date.now(), ...text !== '' && !text.includes('\n') && text.length <= 200 ? { text } : {}, replace })
    showLeft('search')
  }
  const showSourceControl = (focusMessage = false): void => {
    if (state.workspace === null) return
    showLeft('git')
    void refreshGit()
    if (focusMessage) requestAnimationFrame(() => { document.querySelector<HTMLTextAreaElement>('[data-git-message]')?.focus() })
  }
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
      find: () => { run(editor.current?.action('find') ?? Promise.resolve()) },
      replace: () => { run(editor.current?.action('replace') ?? Promise.resolve()) },
      line: () => { run(editor.current?.action('gotoLine') ?? Promise.resolve()) },
      files: () => {
        if (narrow) {
          setNarrowNavigation(!leftVisible)
          model.layout({ sidebarVisible: true })
        } else model.layout({ sidebarVisible: !layout.sidebarVisible })
        setNavigation('files')
      },
      agent: () => { model.layout({ agentVisible: !layout.agentVisible }) },
      bottom: () => { model.layout({ bottomVisible: !layout.bottomVisible }) },
      palette: () => { setPalette('') },
      searchFiles: () => { openSearch(false) },
      replaceFiles: () => { openSearch(true) },
      sourceControl: () => { showSourceControl() },
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
    togglePreview: () => views.length > 0 && showView(currentView === 'preview' ? 'source' : 'preview'),
    previewToSide: () => views.length > 0 && showView('split'),
    canPreview: () => editing && views.length > 0,
    commandPalette: () => { setPalette('') },
    searchFiles: () => { openSearch(false) },
    replaceFiles: () => { openSearch(true) },
    sourceControl: () => { showSourceControl() },
    explorer: () => { showLeft('files') },
  }
  useGlobalShortcuts(shortcuts)
  const showPane = useRef<(pane: string, section: string | undefined) => void>(() => undefined)
  showPane.current = (pane, section) => {
    if (pane === 'ai') model.layout({ agentVisible: true })
    else if (pane === 'files' || pane === 'history') showLeft(pane)
    else if (pane === 'bottom') model.layout({ bottomVisible: true })
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
          { id: 'searchFiles', label: ts('searchInFiles'), disabled: state.workspace === null },
          { id: 'replaceFiles', label: ts('replaceInFiles'), disabled: state.workspace === null },
          { id: 'line', label: t('ideGoLine') },
          { id: 'format', label: t('ideFormat') },
        ]
        : [
          { id: 'palette', label: ta('cmdPalette') },
          { id: 'focus', label: t('ideFocusEditor') },
          { id: 'files', label: t('ideToggleFiles') },
          { id: 'sourceControl', label: ta('cmdSourceControl'), disabled: state.workspace === null },
          { id: 'agent', label: t('ideToggleAgent') },
          { id: 'bottom', label: t('ideToggleBottom') },
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
  const noWorkspace = state.workspace === null
  const sectionLabels: Record<SettingsSection, string> = {
    general: ta('settingsGeneral'), appearance: ta('settingsAppearance'), models: ta('settingsModels'), extensions: ta('settingsExtensions'),
    runtime: ta('settingsRuntime'), memory: ta('settingsMemory'), security: ta('settingsSecurity'), shortcuts: ta('settingsShortcuts'),
  }
  const paletteCommands: PaletteCommand[] = [
    { id: 'file.openFolder', category: ta('catFile'), label: t('ideOpenFolder'), keys: ['Ctrl', '+', 'Alt', '+', 'O'], run: () => { openDirectory('open') } },
    { id: 'file.quickOpen', category: ta('catFile'), label: t('ideQuickOpen'), keys: QUICK_OPEN_KEYS, disabled: noWorkspace, run: () => { model.quickOpen(true) } },
    { id: 'file.new', category: ta('catFile'), label: t('ideNewFile'), disabled: noWorkspace, run: () => { run(create(false)) } },
    { id: 'file.newFolder', category: ta('catFile'), label: t('ideNewFolder'), disabled: noWorkspace, run: () => { run(create(true)) } },
    { id: 'file.save', category: ta('catFile'), label: t('ideSave'), keys: ['Ctrl', '+', 'S'], disabled: activePath === null || active?.source === 'snippet', run: () => { run(model.save()) } },
    { id: 'file.saveAll', category: ta('catFile'), label: t('ideSaveAll'), disabled: activePath === null, run: () => { run(model.saveAll()) } },
    { id: 'file.close', category: ta('catFile'), label: ta('cmdCloseEditor'), disabled: activePath === null, run: () => { if (activePath !== null) run(closeFile(activePath)) } },
    { id: 'edit.find', category: ta('catEdit'), label: t('ideFind'), keys: ['Ctrl', '+', 'F'], disabled: !editing, run: () => { action('find') } },
    { id: 'edit.replace', category: ta('catEdit'), label: t('ideReplace'), keys: ['Ctrl', '+', 'H'], disabled: !editing, run: () => { action('replace') } },
    { id: 'edit.line', category: ta('catEdit'), label: t('ideGoLine'), keys: ['Ctrl', '+', 'G'], disabled: !editing, run: () => { action('line') } },
    { id: 'edit.format', category: ta('catEdit'), label: t('ideFormat'), keys: ['Shift', '+', 'Alt', '+', 'F'], disabled: !editing, run: () => { action('format') } },
    { id: 'edit.compare', category: ta('catEdit'), label: t('ideCompare'), disabled: !editing || active?.source === 'snippet', run: () => { action('compare') } },
    { id: 'edit.searchFiles', category: ta('catEdit'), label: ta('cmdSearch'), keys: ['Ctrl', '+', 'Shift', '+', 'F'], disabled: noWorkspace, run: () => { openSearch(false) } },
    { id: 'edit.replaceFiles', category: ta('catEdit'), label: ta('cmdReplace'), keys: ['Ctrl', '+', 'Shift', '+', 'H'], disabled: noWorkspace, run: () => { openSearch(true) } },
    { id: 'view.palette', category: ta('catView'), label: ta('cmdPalette'), keys: PALETTE_KEYS, run: () => { setPalette('') } },
    { id: 'view.explorer', category: ta('catView'), label: ta('cmdExplorer'), keys: ['Ctrl', '+', 'Shift', '+', 'E'], run: () => { showLeft('files') } },
    { id: 'view.sourceControl', category: ta('catView'), label: ta('cmdSourceControl'), keys: ['Ctrl', '+', 'Shift', '+', 'G'], disabled: noWorkspace, run: () => { showSourceControl() } },
    { id: 'view.files', category: ta('catView'), label: t('ideToggleFiles'), run: () => { action('files') } },
    { id: 'view.bottom', category: ta('catView'), label: t('ideToggleBottom'), run: () => { action('bottom') } },
    { id: 'view.agent', category: ta('catView'), label: t('ideToggleAgent'), run: () => { action('agent') } },
    { id: 'view.focus', category: ta('catView'), label: t('ideFocusEditor'), run: () => { action('focus') } },
    { id: 'view.problems', category: ta('catView'), label: ta('cmdProblems'), run: () => { model.layout({ bottomVisible: true, bottomTab: 'problems' }) } },
    { id: 'view.terminal', category: ta('catView'), label: ta('cmdNewTerminal'), keys: ['Ctrl', '+', '`'], disabled: noWorkspace, run: newTerminal },
    { id: 'view.map', category: ta('catView'), label: ta('cmdMap'), disabled: noWorkspace, run: () => { model.center('map') } },
    { id: 'run.file', category: ta('catRun'), label: ta('cmdRunFile'), disabled: !editing, run: () => { run(launch(false)) } },
    { id: 'run.debug', category: ta('catRun'), label: ta('cmdDebugFile'), disabled: !editing || target?.configuration.language === 'php', run: () => { run(launch(true)) } },
    { id: 'run.profiles', category: ta('catRun'), label: t('ideRunProfiles'), disabled: noWorkspace, run: () => { setRunConfiguration(true) } },
    { id: 'ai.newChat', category: ta('catAi'), label: ta('cmdNewChat'), keys: ['Ctrl', '+', 'Alt', '+', 'N'], run: newChat },
    { id: 'ai.history', category: ta('catAi'), label: ta('cmdHistory'), keys: ['Ctrl', '+', 'Alt', '+', 'K'], run: shortcuts.searchHistory },
    { id: 'ai.edit', category: ta('catAi'), label: ta('cmdAiEdit'), keys: ['Ctrl', '+', 'K'], disabled: !editing, run: () => { void editor.current?.action('rainy.aiEdit') } },
    { id: 'ai.selection', category: ta('catAi'), label: t('ideSendSelection'), disabled: state.selection === undefined, run: () => { workbench.sendSelection() } },
    { id: 'ai.suggest', category: ta('catAi'), label: ta('cmdToggleSuggest'), run: () => { setPrefs({ inlineCompletion: !prefs.inlineCompletion }).catch(() => undefined) } },
    { id: 'git.commit', category: ta('catGit'), label: ta('cmdGitCommit'), disabled: noWorkspace, run: () => { showSourceControl(true) } },
    { id: 'git.refresh', category: ta('catGit'), label: ta('cmdGitRefresh'), disabled: noWorkspace, run: () => { void refreshGit() } },
    { id: 'settings.open', category: ta('catSettings'), label: ta('settings'), keys: ['Ctrl', '+', ','], run: () => { openSettings('general') } },
    ...(Object.keys(sectionLabels) as SettingsSection[]).map(section => ({
      id: `settings.${section}`, category: ta('catSettings'), label: sectionLabels[section], run: () => { openSettings(section) },
    })),
    { id: 'remote.ssh', category: ta('catSettings'), label: ta('cmdSsh'), run: () => { openSettings('runtime') } },
    { id: 'settings.themeLight', category: ta('catSettings'), label: ta('cmdThemeLight'), run: () => { setPrefs({ theme: 'light' }).catch(() => undefined) } },
    { id: 'settings.themeDark', category: ta('catSettings'), label: ta('cmdThemeDark'), run: () => { setPrefs({ theme: 'dark' }).catch(() => undefined) } },
    { id: 'settings.themeSystem', category: ta('catSettings'), label: ta('cmdThemeSystem'), run: () => { setPrefs({ theme: 'system' }).catch(() => undefined) } },
    { id: 'settings.language', category: ta('catSettings'), label: ta('cmdLanguage'), run: () => { setPrefs({ locale: prefs.locale === 'zh' ? 'en' : 'zh' }).catch(() => undefined) } },
    { id: 'help.menu', category: ta('catHelp'), label: ta('cmdHelp'), run: help },
  ]

  return (
    <div className={css.app} data-app-frame>
      <header className={css.topbar} data-window-drag data-rainy-topbar aria-label={ta('appMenu')}>
        <div className={css.topStart}>
          <RainyMark size={18} raining={agentBusy} className={css.topMark} />
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
            <IconAction label={tm('mapTitle')} pressed={state.center === 'map'} disabled={state.workspace === null} onClick={() => { model.center('map') }}>
              <IconMapOutline size={16} />
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
          {navigation !== 'history' && <div className={css.sideTabs} role="group" aria-label={t('ideWorkspace')}>
            {([['files', ta('sideExplorer'), <IconFilesOutline key="files" size={16} />], ['search', ts('searchTitle'), <IconSearchOutlineRegular key="search" size={16} />],
              ['git', tg('gitTitle'), <IconSourceControlOutline key="git" size={16} />]] as const).map(([id, label, icon]) => (
              <Tooltip key={id} label={label} side="bottom" portal>
                <button type="button" className={css.sideTab} aria-pressed={navigation === id} aria-label={label} data-side-tab={id}
                  onClick={() => { if (id === 'search') openSearch(false); else if (id === 'git') showSourceControl(); else showLeft('files') }}>
                  {icon}
                  {id === 'git' && gitChanges > 0 && <span className={css.sideBadge} data-git-badge>{gitChanges > 999 ? '999+' : gitChanges}</span>}
                </button>
              </Tooltip>
            ))}
          </div>}
          <div className={css.explorerHeader}>
            {navigation === 'history' && <IconAction label={t('ideBackToFiles')} onClick={() => { setNavigation('files') }}>
              <IconChevronLeftOutlineRegular size={14} />
            </IconAction>}
            <span className={css.paneTitle} title={state.workspace?.path}>
              {navigation === 'history' ? t('ideHistory') : navigation === 'search' ? ts('searchTitle') : navigation === 'git' ? tg('gitTitle')
                : (state.workspace?.title ?? t('ideExplorer'))}
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
              }} marks={treeMarks} />
          </div>
          <div className={css.retained} hidden={navigation !== 'search'}>
            <SearchView model={model} state={state} focus={searchFocus} />
          </div>
          {navigation === 'git' && leftVisible && <div className={css.retained}>
            <GitView model={model} state={state} />
          </div>}
          <div className={css.retained} hidden={navigation !== 'history'} ref={history}>
            <History currentSessionId={sessionId} workspaceId={state.workspace?.workspaceId ?? null} onNewChat={newChat}
              onSelect={(summary) => { setNarrowNavigation(false); void workbench.openSession(summary) }} />
          </div>
        </aside>
        <main className={`${css.pane} ${css.center}`}>
          <div className={css.editorArea}>
            <div className={css.editorHeader} hidden={state.data.tabs.length === 0 && !mapOpen}>
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
                {mapOpen && <div className={css.tab} data-active={state.center === 'map' || undefined}>
                  <button type="button" role="tab" aria-selected={state.center === 'map'} className={css.button}
                    onClick={() => { model.center('map') }}><IconMapOutline size={14} /><span className={css.tabName}>{tm('mapTitle')}</span></button>
                  <IconAction label={tm('mapClose')} onClick={() => { setMapOpen(false); if (state.center === 'map') model.center('editor') }}>
                    <IconCloseOutlineRegular size={12} />
                  </IconAction>
                </div>}
              </div>
              {editing && <div className={css.editorActions}>
                {views.length > 0 && <div className={css.viewSwitch} role="group" aria-label={tv('viewPreview')}>
                  {views.map(view => (
                    <button key={view} type="button" className={css.viewButton} aria-pressed={currentView === view}
                      title={tv(view === 'rich' ? 'viewRichTitle' : view === 'source' ? 'viewSourceTitle' : view === 'preview' ? 'viewPreviewTitle' : 'viewSplitTitle')}
                      onClick={() => { showView(view) }}>
                      {tv(view === 'rich' ? 'viewRich' : view === 'source' ? 'viewSource' : view === 'preview' ? 'viewPreview' : 'viewSplit')}
                    </button>
                  ))}
                </div>}
                {active.source !== 'snippet' && <button type="button" className={css.viewButton} aria-pressed={tab?.view === 'hex'} title={tv('viewHexTitle')}
                  onClick={() => { if (activePath !== null) model.setView(activePath, tab?.view === 'hex' ? undefined : 'hex') }}>{tv('viewHex')}</button>}
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
            {reviewing !== undefined && activePath !== null && (
              <ReviewBar remaining={reviewing.remaining} busy={reviewBusy}
                onPrevious={() => { void editor.current?.action('rainy.previousChange') }}
                onNext={() => { void editor.current?.action('rainy.nextChange') }}
                onKeep={() => { void editor.current?.action('rainy.keepAll') }}
                onUndo={() => { reviewStep(() => undoReview(model, editor.current, activePath)) }}
                onClose={() => { model.endReview(activePath) }} />
            )}
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
                review={(path, remaining) => { if (remaining === 0) reviewStep(() => finishReview(model, path)) }}
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
            <div className={css.centerBody} hidden={state.center !== 'map'}>
              {mapOpen && <Suspense fallback={<div className={css.notice} role="status">{tm('mapLoading')}</div>}>
                <ProjectMapView workspace={state.workspace} visible={state.center === 'map'} changes={changes} activity={aiActivity}
                  openFile={(path) => { run(model.openFile(path)) }} ask={askAi} />
              </Suspense>}
            </div>
          </div>
          <div className={css.bottom} hidden={!layout.bottomVisible}
            style={{ height: layout.bottomVisible ? bottomHeight : 0, minHeight: layout.bottomVisible ? undefined : 0 }}>
            {resize('bottom')}
            <IdeBottom state={state} executionState={executionState} execution={execution} model={model} appearance={appearance}
              reveal={(path, line, column) => { run(model.reveal(path, line, column)) }} ask={askAi} />
          </div>
        </main>
        <aside className={`${css.pane} ${css.agent}`} hidden={!layout.agentVisible} aria-label={t('ideAgent')}>
          <div className={css.retained} data-rainy-agent>
            <ChatPane workspace={state.workspace} sessionId={sessionId}
              onSessionChange={showSession}
              onClose={() => { model.layout({ agentVisible: false }) }}
              onShowHistory={showHistory} />
          </div>
        </aside>
        <footer className={css.statusBar} data-rainy-status>
          {environment !== undefined && <span className={css.statusItem} title={`${environment.name} ${environment.version}`}>
            <span className={css.statusDot} aria-hidden="true" />{environment.label ?? environment.environment}
          </span>}
          {state.saving && <span className={css.statusItem}>{t('ideRecoverySaving')}</span>}
          {gitBranch !== undefined && <button type="button" className={`${css.statusItem} ${css.statusButton}`} title={tg('gitPanel')} data-git-branch
            onClick={() => { showSourceControl() }}>
            <IconBranchOutlineRegular size={12} />{gitBranch.branch ?? gitBranch.head ?? tg('gitNoCommits')}
            {gitChanges > 0 && <span className={css.statusCount}>{gitChanges}</span>}
          </button>}
          {activity !== undefined && <span className={css.statusItem} data-activity>{activity}</span>}
          <span className={css.spacer} />
          {caret !== undefined && <span className={css.statusItem}>{t('ideStatusPosition', { line: caret.endLine, column: caret.endColumn })}</span>}
          {caret !== undefined && caret.text.length > 0 && <span className={css.statusItem}>{t('ideStatusSelected', { count: caret.text.length })}</span>}
          {active !== undefined && (active.document.content !== null ? <>
            <button type="button" className={`${css.statusItem} ${css.statusButton}`} aria-pressed={prefs.inlineCompletion} title={t('ideSuggestToggle')}
              data-suggest-toggle onClick={() => { setPrefs({ inlineCompletion: !prefs.inlineCompletion }).catch(() => undefined) }}>
              {t(prefs.inlineCompletion ? 'ideSuggestOn' : 'ideSuggestOff')}
            </button>
            <span className={css.statusItem}>{active.language ?? sourceLanguage(active.document.path)}</span>
            <span className={css.statusItem}>{active.document.eol.toUpperCase()}</span>
            <span className={css.statusItem}>{t('ideEncoding')}{active.document.bom ? ` ${t('ideBom')}` : ''}</span>
          </> : <>
            {active.decoded !== undefined && <span className={css.statusItem}>{active.decoded.encoding.toUpperCase()}</span>}
            <span className={css.statusItem}>{fileSizeText(active.document.bytes)}</span>
          </>)}
        </footer>
        {leftVisible && !narrow && resize('left', leftWidth)}
        {layout.agentVisible && resize('right', width - rightWidth)}
      </div>
      <PromptDialog prompt={prompt} close={resolvePrompt} />
      <QuickOpenDialog model={model} state={state} onCommands={(query) => { model.quickOpen(false); setPalette(query) }} />
      <CommandPalette query={palette} commands={paletteCommands} onClose={() => { setPalette(undefined) }} />
      <DirectoryBrowser open={directoryOpen} busy={directoryBusy} onOpen={adoptDirectory} onClose={() => { setDirectoryOpen(false) }}
        listDirectory={listDirectory} createDirectory={createDirectory} />
      <RunConfigurationDialog open={runConfiguration} close={() => { setRunConfiguration(false) }} state={state} model={model} />
      <SettingsDialog open={settings.open} section={settings.section} workspace={state.workspace} onClose={() => { setSettings({ open: false }) }} />
    </div>
  )
}
