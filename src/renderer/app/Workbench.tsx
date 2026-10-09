/** Workspace editor with optional retained chat, tools, and execution panels. */
import { useEffect, useRef, useState } from 'react'
import type { PointerEvent } from 'react'
import {
  Button,
  Menu,
  Modal,
  Tooltip,
  ShortcutKeys,
  type MenuEntry,
  IconCheckOutlineRegular,
  IconChevronDownOutlineRegular,
  IconChevronLeftOutlineRegular,
  IconChevronRightOutlineRegular,
  IconCloseOutlineRegular,
  IconClockOutlineRegular,
  IconEllipsisOutlineRegular,
  IconFolderCloseRegular,
  IconNewChatOutlineRegular,
  IconPanelLeftOutlineRegular,
  IconPlusOutlineRegular,
  IconSearchOutlineRegular,
  IconPlayOutlineRegular,
  IconProjectAddOutlineRegular,
  FileTypeIcon,
  IconShieldOutlineRegular,
  IconSettingsOutlineRegular,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type {
  InjectFace,
  PropsLocale,
  PropsRenderFactories,
  PropsRenderSlots,
  PropsRuntime,
} from '@deepseek-ai/dsh-client-ui-slots'
import type { IdeShellInjected } from './ide.tsx'
import type { IdeFileEntry, IdeRootId } from '../ide-files-protocol.ts'
import type { EditorInstance } from './editor-types.ts'
import { sourceLanguage, type IdeState } from './ide-model.ts'
import { IdeEditor } from './IdeEditor.tsx'
import { IdeBottom } from './IdeBottom.tsx'
import { RunConfigurationDialog } from './RunConfigurationDialog.tsx'
import { QuickOpenDialog } from './QuickOpenDialog.tsx'
import type { IdeRunConfiguration } from '../ide-execution-protocol.ts'
import type {} from '@deepseek-ai/dsh-client-ui-directory-picker-browse/client'
import css from './IdeShell.module.css'
import { IconBugOutline, IconPanelBottomOutline, IconPanelRightOutline } from './icons.tsx'
import { IconAction } from './IconAction.tsx'
import { chooseFileLanguage, inferredRunLanguage, pinRunProfile, runLanguageNames, runLanguages, runsFile, runTarget, type RunFile } from './run-target.ts'

declare global {
  interface Window {
    /** Carrier identity injected into the Host index; absent outside the desktop Host. */
    __RAINY_AGENT__?: { readonly name: string; readonly version: string; readonly environment: string }
  }
}
import { fileKey, fileLabel, fileReference, keyFromAbsolute, workspaceRoots } from './ide-paths.ts'

type Props = PropsRuntime<'shell.workspace'> &
  PropsLocale<'rainy'> &
  PropsRenderFactories &
  PropsRenderSlots<'rainy.ide.tools'> &
  InjectFace<IdeShellInjected>
interface Prompt {
  readonly title: string
  readonly description?: string
  readonly initial?: string
  readonly dirty?: boolean
  readonly resolve: (value: string | null) => void
}

function FileTree({
  state,
  directory,
  level,
  selected,
  select,
  activate,
}: {
  state: IdeState
  directory: string
  level: number
  selected: string | undefined
  select: (entry: IdeFileEntry) => void
  activate: (entry: IdeFileEntry) => void
}) {
  const entries = [...(state.directories[directory]?.entries ?? [])].sort((left, right) => {
    const a = left.kind === 'directory' || left.targetKind === 'directory'
    const b = right.kind === 'directory' || right.targetKind === 'directory'
    return Number(b) - Number(a) || left.name.localeCompare(right.name)
  })
  return entries.map((entry) => {
    const folder = entry.kind === 'directory' || entry.targetKind === 'directory'
    const expanded = state.data.expandedPaths.includes(entry.path)
    return (
      <div key={entry.path}>
        <div className={css.treeRow} data-selected={selected === entry.path || undefined}>
          <button
            type="button"
            className={`${css.button} ${css.treeButton}`}
            style={{ paddingLeft: 9 + level * 14 }}
            role="treeitem"
            aria-expanded={folder ? expanded : undefined}
            aria-selected={selected === entry.path}
            title={fileLabel(state.workspace, entry.path)}
            disabled={state.phase === 'loading' || entry.outsideWorkspace || entry.kind === 'other'}
            onClick={() => {
              select(entry)
              activate(entry)
            }}
            onContextMenu={(event) => {
              event.preventDefault()
              select(entry)
            }}
          >
            <span className={css.treeIcon} aria-hidden>
              {folder ? (expanded ? <IconChevronDownOutlineRegular size={14} /> : <IconChevronRightOutlineRegular size={14} />)
                : <FileTypeIcon path={entry.name} size={16} />}
            </span>
            <span className={css.treeName}>{entry.name}</span>
          </button>
        </div>
        {folder && expanded && (
          <FileTree
            state={state}
            directory={entry.path}
            level={level + 1}
            selected={selected}
            select={select}
            activate={activate}
          />
        )}
      </div>
    )
  })
}

function PromptDialog({
  prompt,
  close,
  t,
}: {
  prompt: Prompt | undefined
  close: (result: string | null) => void
  t: Props['t']
}) {
  const [value, setValue] = useState('')
  useEffect(() => {
    setValue(prompt?.initial ?? '')
  }, [prompt])
  return (
    <Modal
      open={prompt !== undefined}
      className={`${css.promptDialog}`}
      contentClassName={`${css.dialogContent}`}
      onClose={() => {
        close(null)
      }}
      title={prompt?.title ?? ''}
      description={prompt?.description ?? ''}
      closeLabel={t('ideClose')}
      footer={
        <>
          <Button
            onClick={() => {
              close(null)
            }}
          >
            {t('ideCancel')}
          </Button>
          {prompt?.dirty && (
            <Button
              onClick={() => {
                close('discard')
              }}
            >
              {t('ideDiscard')}
            </Button>
          )}
          <Button
            onClick={() => {
              close(prompt?.dirty ? 'save' : prompt?.initial === undefined ? 'confirm' : value)
            }}
          >
            {t(prompt?.dirty ? 'ideSave' : 'ideConfirm')}
          </Button>
        </>
      }
    >
      {prompt?.initial !== undefined && (
        <label className={css.field}>
          {t('idePath')}
          <input
            className={css.input}
            data-modal-autofocus
            value={value}
            onChange={(event) => {
              setValue(event.target.value)
            }}
            onKeyDown={(event) => {
              if (event.key === 'Enter') close(value)
            }}
          />
        </label>
      )}
    </Modal>
  )
}

/** First view without a project: the two ways to open one, then recently registered projects. */
function Welcome({ state, t, nativeDirectory, pending, open, browse, select }: {
  state: IdeState
  t: Props['t']
  nativeDirectory: boolean
  pending: boolean
  open: () => void
  browse: () => void
  select: (workspace: IdeState['workspaces'][number]) => void
}) {
  return <div className={css.welcome}>
    <div className={css.welcomeBody}>
      <img src="/rainy/icon.png" alt="" className={css.welcomeMark} />
      <h2 className={css.welcomeTitle}>{t('ideWelcomeTitle')}</h2>
      <p className={css.welcomeText}>{t('ideWelcomeDescription')}</p>
      <div className={css.welcomeActions}>
        {nativeDirectory && <Button variant="primary" disabled={pending} onClick={open}>{t('ideOpenFolder')}</Button>}
        <Button variant="outline" disabled={pending} onClick={browse}>{t('ideWslFolder')}</Button>
      </div>
      {state.workspaces.length > 0 && <section className={css.recent} aria-label={t('ideRecentProjects')}>
        <h3>{t('ideRecentProjects')}</h3>
        {state.workspaces.slice(0, 6).map(workspace => (
          <button key={workspace.workspaceId} type="button" className={css.recentItem} title={workspace.path}
            disabled={state.phase === 'loading'} onClick={() => { select(workspace) }}>
            <IconFolderCloseRegular size={14} />
            <span className={css.recentName}>{workspace.title}</span>
            <span className={css.recentPath}>{workspace.path}</span>
          </button>
        ))}
      </section>}
    </div>
  </div>
}

/** A project without an open file offers search, a new file, and the assistant instead of a blank editor. */
function NoFile({ t, shortcut, search, create, agent }: {
  t: Props['t']
  shortcut: readonly string[]
  search: () => void
  create: () => void
  agent: (() => void) | undefined
}) {
  return <div className={css.welcome}>
    <div className={css.welcomeBody}>
      <h2 className={css.welcomeTitle}>{t('ideNoFileTitle')}</h2>
      <p className={css.welcomeText}>{t('ideNoFile')}</p>
      <div className={css.shortcutList}>
        <button type="button" className={css.shortcutRow} onClick={search}>
          <span>{t('ideSearchFiles')}</span>{shortcut.length > 0 && <ShortcutKeys keys={shortcut} className={css.shortcut} />}
        </button>
        <button type="button" className={css.shortcutRow} onClick={create}><span>{t('ideCreateFile')}</span></button>
        {agent !== undefined && <button type="button" className={css.shortcutRow} onClick={agent}><span>{t('ideOpenAgent')}</span></button>}
      </div>
    </div>
  </div>
}

/** Compose the editor, real conversation, retained history, tools, and interactive execution panels.
 * @param props Slot geometry, locale, and private workspace controllers.
 * @returns The complete IDE shell.
 */
export function IdeShell({
  viewportWidth,
  useIde,
  useAppearance,
  useExecution,
  useQuickOpenShortcut,
  useDirectoryPending,
  model,
  execution,
  openFolder,
  nativeDirectory,
  newChat,
  settings,
  sendSelection,
  renderFactorySlot,
  renderSlot,
  t,
}: Props) {
  const state = useIde(value => value)
  const appearance = useAppearance(value => value)
  const executionState = useExecution(value => value)
  const quickOpenKeys = useQuickOpenShortcut(value => value)
  const directoryPending = useDirectoryPending(value => value)
  const [navigation, setNavigation] = useState<'files' | 'history'>('files')
  const [selected, setSelected] = useState<IdeFileEntry | undefined>()
  const [menu, setMenu] = useState<'file' | 'edit' | 'view' | 'explorer' | 'editor' | 'app' | 'workspace' | 'run' | undefined>()
  const [prompt, setPrompt] = useState<Prompt | undefined>()
  const [runConfiguration, setRunConfiguration] = useState(false)
  const [directoryOpen, setDirectoryOpen] = useState(false)
  const [directoryBusy, setDirectoryBusy] = useState(false)
  const [directoryMode, setDirectoryMode] = useState<'open' | 'attach'>('open')
  const [selectedRootId, setSelectedRootId] = useState<IdeRootId | undefined>()
  const [narrowNavigation, setNarrowNavigation] = useState(false)
  const [toolsOpen, setToolsOpen] = useState(state.center === 'tools')
  useEffect(() => { if (state.center === 'tools') setToolsOpen(true) }, [state.center])
  const editor = useRef<EditorInstance | undefined>(undefined)
  const drag = useRef<{ side: 'left' | 'right' | 'bottom'; origin: number; size: number } | undefined>(undefined)
  const layout = state.data.layout
  const width = viewportWidth || window.innerWidth
  const narrow = width <= 720 || (width <= 1100 && layout.agentVisible)
  useEffect(() => {
    if (!narrow) setNarrowNavigation(false)
  }, [narrow])
  const leftVisible = layout.sidebarVisible && (!narrow || narrowNavigation)
  // Drag, keyboard and rendering share these bounds so a stored size never hides the editor.
  const sidebarSize = (value: number): number => Math.min(360, Math.max(180, value))
  const leftPanelWidth = sidebarSize(layout.sidebarWidth)
  const leftWidth = leftVisible && !narrow ? leftPanelWidth : 0
  const agentSize = (value: number): number => Math.max(300, Math.min(width - leftWidth - 300, value))
  const bottomSize = (value: number): number => Math.max(120, Math.min(window.innerHeight - 230, value))
  const rightWidth = layout.agentVisible ? agentSize(layout.agentWidth) : 0
  const bottomHeight = bottomSize(layout.bottomHeight)
  const centerWidth = Math.max(0, width - leftWidth - rightWidth)
  const activePath = state.data.activePath
  const roots = state.workspace === null ? [] : workspaceRoots(state.workspace)
  const active = activePath === null ? undefined : state.buffers[activePath]
  const tab = state.data.tabs.find(entry => entry.path === activePath)
  const editing = state.center === 'editor' && active !== undefined
  const debug = executionState.status.debugSessions.find(entry => entry.id === executionState.debugId)
  const debugFrame = executionState.frames.find(frame => frame.id === executionState.frameId)
  const stoppedPath = debugFrame?.path !== undefined && state.workspace !== null
    ? keyFromAbsolute(state.workspace, debugFrame.path) ?? (debugFrame.path.startsWith('/') ? undefined : debugFrame.path)
    : undefined
  const environment = typeof window === 'undefined' ? undefined : window.__RAINY_AGENT__
  const caret = state.selection !== undefined && state.selection.path === activePath && state.center === 'editor' ? state.selection : undefined
  const runningProgram = executionState.status.runs.find(entry => entry.phase === 'starting' || entry.phase === 'building' || entry.phase === 'running')
  const activity = debug !== undefined && debug.phase !== 'terminated' && debug.phase !== 'failed'
    ? `${t('ideDebugging')} · ${debug.name}`
    : runningProgram === undefined ? undefined : `${t('ideRunning')} · ${runningProgram.name}`
  const stopped =
    debug?.phase === 'paused' && stoppedPath !== undefined && debugFrame !== undefined
      ? { path: stoppedPath, line: debugFrame.line }
      : undefined
  const run = (operation: Promise<unknown>): void => {
    void operation.catch((error: unknown) => {
      model.fail(error)
    })
  }
  const openDirectory = (mode: 'open' | 'attach' = 'open'): void => {
    if (directoryPending) return
    setDirectoryMode(mode)
    if (nativeDirectory) run(openFolder(mode))
    else setDirectoryOpen(true)
  }
  const help = (): void => {
    window.dispatchEvent(new CustomEvent('rainy:native-menu', { detail: { menu: 'help', locale: t('locale') } }))
  }
  const adoptDirectory = (path: string): void => {
    setDirectoryBusy(true)
    void (directoryMode === 'attach' ? model.attachRoot(path) : model.openWorkspace(path))
      .then(() => { setDirectoryOpen(false) })
      .catch((error: unknown) => { setDirectoryOpen(false); model.fail(error) })
      .finally(() => { setDirectoryBusy(false) })
  }
  const request = (next: Omit<Prompt, 'resolve'>): Promise<string | null> =>
    new Promise((resolve) => {
      setPrompt({ ...next, resolve })
    })
  const resolvePrompt = (value: string | null): void => {
    prompt?.resolve(value)
    setPrompt(undefined)
  }
  useEffect(
    () => () => {
      prompt?.resolve(null)
    },
    [prompt],
  )
  useEffect(() => {
    setSelected(undefined)
    setSelectedRootId(undefined)
  }, [state.workspace?.workspaceId])
  const closeFile = async (path: string): Promise<void> => {
    if (state.buffers[path]?.dirty) {
      const result = await request({
        title: t('ideDirtyTitle'),
        description: `${fileLabel(state.workspace, path)}\n${t('ideDirtyDescription')}`,
        dirty: true,
      })
      if (
        result === null ||
        (result === 'save' && (!(await model.save(path)) || model.state.getSnapshot().buffers[path]?.dirty))
      )
        return
    }
    model.close(path)
  }
  const reloadFile = async (): Promise<void> => {
    if (activePath === null) return
    if (active?.dirty && (await request({ title: t('ideDiscard'), description: fileLabel(state.workspace, activePath) })) === null) return
    await model.reload(activePath)
  }
  const create = async (directory: boolean): Promise<void> => {
    const reference = selected === undefined ? { path: '', rootId: selectedRootId } : fileReference(selected.path)
    const folder =
      selected === undefined
        ? ''
        : selected.kind === 'directory' || selected.targetKind === 'directory'
          ? reference.path + '/'
          : reference.path.slice(0, reference.path.lastIndexOf('/') + 1)
    const path = await request({ title: t(directory ? 'ideNewFolder' : 'ideNewFile'), initial: folder })
    if (path !== null && path.trim() !== '') await model.create(fileKey(path.trim(), reference.rootId), directory)
  }
  const rename = async (): Promise<void> => {
    if (selected === undefined) return
    const reference = fileReference(selected.path)
    const path = await request({ title: t('ideRename'), initial: reference.path })
    if (path !== null && path.trim() !== '' && path !== reference.path) await model.rename(selected, fileKey(path.trim(), reference.rootId))
  }
  const remove = async (): Promise<void> => {
    if (selected === undefined) return
    await model.remove(
      selected.path,
      async (entries, bytes) =>
        (await request({
          title: t('ideDelete'),
          description: t('ideDeleteDescription', { path: fileLabel(state.workspace, selected.path), entries, bytes }),
        })) !== null,
    )
  }
  const activeReference = activePath === null ? undefined : fileReference(activePath)
  const runFile: RunFile | undefined = activePath === null || activeReference === undefined ? undefined
    : { path: activePath, label: fileLabel(state.workspace, activePath), program: activeReference.path,
      ...activeReference.rootId === undefined ? {} : { rootId: activeReference.rootId } }
  const target = runTarget(state.data.execution, runFile)
  const configuration = (): IdeRunConfiguration | undefined => target?.configuration
  const inferred = activePath === null ? undefined : inferredRunLanguage(activePath)
  const fileLanguage = target !== undefined && target.mode !== 'pinned' ? target.configuration.language : undefined
  const checked = (on: boolean) => on ? <IconCheckOutlineRegular size={14} /> : <span className={css.menuCheck} />
  const otherProfiles = (state.data.execution?.profiles ?? [])
    .filter(profile => runFile === undefined || !runsFile(profile, runFile)).slice(0, 8)
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
    const execution = state.data.execution
    if (id === 'edit') setRunConfiguration(true)
    else if (id.startsWith('profile:')) model.execution(pinRunProfile(execution, id.slice('profile:'.length)))
    else if (runFile !== undefined) {
      const choice = id === 'auto' ? 'auto' : runLanguages.find(language => id === `language:${language}`)
      if (choice !== undefined) model.execution(chooseFileLanguage(execution, runFile, choice))
    }
  }
  const runLabel = target === undefined ? t('ideRun') : t('ideRunWith', {
    target: target.mode === 'pinned' ? target.configuration.name : runLanguageNames[target.configuration.language] })
  const launch = async (debug: boolean): Promise<void> => {
    const config = configuration()
    if (config === undefined) {
      setRunConfiguration(true)
      return
    }
    if (!(await model.saveAll())) return
    model.layout({ bottomVisible: true, bottomTab: debug ? 'debug' : 'terminal' })
    if (debug) await execution.debug(config)
    else await execution.run(config)
  }
  const action = (id: string): void => {
    setMenu(undefined)
    const actions: Record<string, () => void> = {
      folder: () => {
        openDirectory('open')
      },
      wsl: () => {
        if (directoryPending) return
        setDirectoryMode('open')
        setDirectoryOpen(true)
      },
      quickOpen: () => {
        model.quickOpen(true)
      },
      profiles: () => {
        setRunConfiguration(true)
      },
      file: () => {
        run(create(false))
      },
      directory: () => {
        run(create(true))
      },
      save: () => {
        run(model.save())
      },
      saveAll: () => {
        run(model.saveAll())
      },
      rename: () => { run(rename()) },
      delete: () => { run(remove()) },
      refresh: () => { run(model.refreshTree()) },
      openFolder: () => { openDirectory('open') },
      attachFolder: () => { openDirectory('attach') },
      help,
      history: () => {
        setNavigation('history')
        setNarrowNavigation(narrow)
        model.layout({ sidebarVisible: true, agentVisible: true })
      },
      compare: () => {
        if (activePath === null) return
        if (tab?.kind === 'diff') model.edit(activePath)
        else run(model.diff(activePath))
      },
      selection: () => { run(sendSelection()) },
      focus: () => { model.layout({ agentVisible: false, bottomVisible: false }); model.center('editor') },
      format: () => {
        run(model.format())
      },
      find: () => {
        run(editor.current?.action('actions.find') ?? Promise.resolve())
      },
      replace: () => {
        run(editor.current?.action('editor.action.startFindReplaceAction') ?? Promise.resolve())
      },
      line: () => {
        run(editor.current?.action('editor.action.gotoLine') ?? Promise.resolve())
      },
      files: () => {
        if (narrow) {
          setNarrowNavigation(!leftVisible)
          model.layout({ sidebarVisible: true })
        } else model.layout({ sidebarVisible: !layout.sidebarVisible })
        setNavigation('files')
      },
      agent: () => {
        model.layout({ agentVisible: !layout.agentVisible })
      },
      bottom: () => {
        model.layout({ bottomVisible: !layout.bottomVisible })
      },
      settings,
      tools: () => {
        model.center('tools')
      },
    }
    actions[id]?.()
  }
  const beginDrag = (event: PointerEvent<HTMLDivElement>, side: 'left' | 'right' | 'bottom'): void => {
    event.currentTarget.setPointerCapture(event.pointerId)
    drag.current = {
      side,
      origin: side === 'bottom' ? event.clientY : event.clientX,
      size: side === 'left' ? leftWidth : side === 'right' ? rightWidth : bottomHeight,
    }
  }
  const moveDrag = (event: PointerEvent<HTMLDivElement>): void => {
    const current = drag.current
    if (current === undefined) return
    const delta = (current.side === 'bottom' ? event.clientY : event.clientX) - current.origin
    if (current.side === 'left') model.layout({ sidebarWidth: sidebarSize(current.size + delta) })
    else if (current.side === 'right') model.layout({ agentWidth: agentSize(current.size - delta) })
    else model.layout({ bottomHeight: bottomSize(current.size - delta) })
  }
  const resize = (side: 'left' | 'right' | 'bottom', position?: number) => (
    <div
      role="separator"
      tabIndex={0}
      aria-orientation={side === 'bottom' ? 'horizontal' : 'vertical'}
      aria-label={t(side === 'left' ? 'ideResizeFiles' : side === 'right' ? 'ideResizeAgent' : 'ideResizeBottom')}
      className={side === 'bottom' ? css.resizeBottom : css.resize}
      style={position === undefined ? undefined : { left: position - 2 }}
      onPointerDown={(event) => {
        beginDrag(event, side)
      }}
      onPointerMove={moveDrag}
      onPointerUp={() => {
        drag.current = undefined
      }}
      onLostPointerCapture={() => {
        drag.current = undefined
      }}
      onKeyDown={(event) => {
        const delta =
          event.key === 'ArrowLeft' || event.key === 'ArrowUp'
            ? -16
            : event.key === 'ArrowRight' || event.key === 'ArrowDown'
              ? 16
              : 0
        if (delta === 0) return
        event.preventDefault()
        if (side === 'left') model.layout({ sidebarWidth: sidebarSize(leftWidth + delta) })
        else if (side === 'right') model.layout({ agentWidth: agentSize(rightWidth - delta) })
        else model.layout({ bottomHeight: bottomSize(bottomHeight - delta) })
      }}
    />
  )
  const menuItems = (kind: 'file' | 'edit' | 'view') =>
    kind === 'file'
      ? [
        { id: 'folder', label: t('ideWindowsFolder'), disabled: !nativeDirectory || directoryPending },
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
  return (
    <div
      className={css.root}
      data-rainy-ide
      style={{ gridTemplateColumns: `${leftWidth}px minmax(0, 1fr) ${rightWidth}px` }}
    >
      <header className={css.topbar} data-window-drag data-rainy-topbar aria-label={t('appMenu')}>
        <div className={css.topStart}>
          <img src="/rainy/icon.png" alt="" className={css.topMark} />
          {width < 900 ? <Menu open={menu === 'app'} onClose={() => { setMenu(undefined) }} portal compact
            anchor={<IconAction label={t('appMenu')} expanded={menu === 'app'} onClick={() => { setMenu(menu === 'app' ? undefined : 'app') }}>
              <IconEllipsisOutlineRegular size={16} />
            </IconAction>}
            items={[
              { id: 'file-menu', label: t('fileMenu'), submenu: menuItems('file') },
              { id: 'edit-menu', label: t('editMenu'), submenu: menuItems('edit') },
              { id: 'view-menu', label: t('viewMenu'), submenu: menuItems('view') },
              { id: 'help', label: t('helpMenu') },
            ]} onSelect={action} /> : <>
            {(['file', 'edit', 'view'] as const).map(kind => (
              <Menu
                key={kind}
                open={menu === kind}
                onClose={() => {
                  setMenu(undefined)
                }}
                portal
                anchor={
                  <button
                    type="button"
                    className={css.button}
                    aria-haspopup="menu"
                    aria-expanded={menu === kind}
                    onClick={() => {
                      setMenu(menu === kind ? undefined : kind)
                    }}
                  >
                    {t(kind === 'file' ? 'fileMenu' : kind === 'edit' ? 'editMenu' : 'viewMenu')}
                  </button>
                }
                items={menuItems(kind)}
                onSelect={action}
              />
            ))}
            <button
              type="button"
              className={css.button}
              onClick={help}
            >
              {t('helpMenu')}
            </button>
          </>}
        </div>
        <div className={css.commandCenter}>
          <Menu open={menu === 'workspace'} onClose={() => { setMenu(undefined) }} portal compact className={css.commandMenu}
            anchor={<button type="button" className={css.commandWorkspace} aria-label={t('ideWorkspace')} title={state.workspace?.path}
              aria-haspopup="menu" aria-expanded={menu === 'workspace'} disabled={state.phase === 'loading'}
              onClick={() => { setMenu(menu === 'workspace' ? undefined : 'workspace') }}>
              <IconFolderCloseRegular size={14} />
              <span>{state.workspace?.title ?? t('ideOpenFolder')}</span>
              <IconChevronDownOutlineRegular size={12} />
            </button>}
            items={[...state.workspaces.map(workspace => ({ id: workspace.workspaceId, label: workspace.title })),
              { id: 'rainy:open-folder', label: t('ideOpenFolder') }]}
            onSelect={(id) => {
              setMenu(undefined)
              if (id === 'rainy:open-folder') openDirectory()
              else {
                const workspace = state.workspaces.find(item => item.workspaceId === id)
                if (workspace !== undefined) run(model.selectWorkspace(workspace))
              }
            }} />
          <Tooltip label={t('ideQuickOpen')} shortcutKeys={quickOpenKeys.length === 0 ? undefined : quickOpenKeys} side="bottom" portal>
            <button type="button" className={css.commandSearch} aria-label={t('ideQuickOpen')} disabled={state.workspace === null}
              onClick={() => { model.quickOpen(true) }}>
              <IconSearchOutlineRegular size={14} />
              <span className={css.commandLabel}>{t('ideSearchFiles')}</span>
              {quickOpenKeys.length > 0 && <ShortcutKeys keys={quickOpenKeys} className={css.shortcut} />}
            </button>
          </Tooltip>
        </div>
        <div className={css.topEnd}>
          {editing && <div className={css.toolGroup}>
            <IconAction label={runLabel} onClick={() => { run(launch(false)) }}><IconPlayOutlineRegular size={16} /></IconAction>
            {target?.configuration.language !== 'php'
              && <IconAction label={t('ideDebug')} onClick={() => { run(launch(true)) }}><IconBugOutline size={16} /></IconAction>}
            <Menu open={menu === 'run'} onClose={() => { setMenu(undefined) }} portal align="end" compact items={runMenu} onSelect={chooseRun}
              anchor={<IconAction label={t('ideRunMode')} expanded={menu === 'run'} onClick={() => { setMenu(menu === 'run' ? undefined : 'run') }}>
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
            <IconAction label={t('ctf')} pressed={state.center === 'tools'} onClick={() => { model.center('tools') }}>
              <IconShieldOutlineRegular size={16} />
            </IconAction>
            <IconAction label={t('settings')} onClick={settings}>
              <IconSettingsOutlineRegular size={16} />
            </IconAction>
          </div>
        </div>
      </header>
      <aside
        className={`${css.pane} ${css.left}`}
        hidden={!leftVisible}
        aria-label={t('ideWorkspace')}
        style={
          narrow && leftVisible
            ? { position: 'absolute', inset: '0 auto 0 0', width: leftPanelWidth, zIndex: 22 }
            : undefined
        }
      >
        <div className={css.explorerHeader}>
          {navigation === 'history' && <IconAction label={t('ideBackToFiles')} onClick={() => { setNavigation('files') }}>
            <IconChevronLeftOutlineRegular size={14} />
          </IconAction>}
          <span className={css.paneTitle} title={state.workspace?.path}>
            {navigation === 'history' ? t('ideHistory') : (state.workspace?.title ?? t('ideExplorer'))}
          </span>
          {navigation === 'files' && <div className={css.explorerActions}>
            <IconAction label={t('ideAddFolder')} disabled={state.phase === 'loading' || directoryPending}
              onClick={() => { openDirectory('attach') }}>
              <IconProjectAddOutlineRegular size={14} />
            </IconAction>
            <IconAction label={t('ideNewFile')} disabled={state.workspace === null} onClick={() => { run(create(false)) }}>
              <IconPlusOutlineRegular size={14} />
            </IconAction>
            <IconAction label={t('ideNewFolder')} disabled={state.workspace === null} onClick={() => { run(create(true)) }}>
              <IconFolderCloseRegular size={14} />
            </IconAction>
            <Menu open={menu === 'explorer'} onClose={() => { setMenu(undefined) }} portal align="end" compact
              anchor={<IconAction label={t('ideFileActions')} expanded={menu === 'explorer'} onClick={() => { setMenu(menu === 'explorer' ? undefined : 'explorer') }}>
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
          <div className={css.tree} role="tree" aria-label={t('ideExplorer')} aria-busy={state.phase === 'loading'}>
            {roots.map(root => <div key={root.rootId}>
              {roots.length > 1 && <div className={css.rootHeader}>
                <button type="button" className={`${css.button} ${css.rootName}`} title={root.path}
                  data-active={(selectedRootId ?? 'primary') === root.rootId || undefined}
                  onClick={() => { setSelected(undefined); setSelectedRootId(root.rootId) }}>
                  <IconFolderCloseRegular size={14} /><span>{root.title}</span>
                </button>
                {!root.primary && <IconAction label={t('ideRemoveRoot') + ' ' + root.title} onClick={() => {
                  run(model.removeRoot(root.rootId).then(() => {
                    if (selectedRootId === root.rootId) { setSelected(undefined); setSelectedRootId(undefined) }
                  }))
                }}>
                  <IconCloseOutlineRegular size={12} />
                </IconAction>}
              </div>}
              {!root.primary && state.phase === 'ready' && state.directories[fileKey('', root.rootId)] === undefined
                && <p className={`${css.notice} ${css.error}`} role="status">{t('ideUnavailableRoot')}</p>}
              <FileTree state={state} directory={fileKey('', root.rootId)} level={roots.length > 1 ? 1 : 0}
                selected={selected?.path} select={(entry) => { setSelected(entry); setSelectedRootId(root.rootId) }}
                activate={(entry) => {
                  const directory = entry.kind === 'directory' || entry.targetKind === 'directory'
                  run(directory ? model.toggleDirectory(entry.path) : model.openFile(entry.path))
                  if (!directory) setNarrowNavigation(false)
                }} />
            </div>)}
          </div>
        </div>
        <div className={css.retained} hidden={navigation !== 'history'}>
          {renderFactorySlot('layout.region', {
            region: 'navigation',
            width: leftPanelWidth,
            viewportWidth: width,
            collapsed: false,
            canShow: true,
          })}
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
                  <IconAction label={t('ideClose') + ' ' + fileLabel(state.workspace, file.path)} onClick={() => { run(closeFile(file.path)) }}>
                    <IconCloseOutlineRegular size={12} />
                  </IconAction>
                </div>
              ))}
              {toolsOpen && <div className={css.tab} data-active={state.center === 'tools' || undefined}>
                <button type="button" role="tab" aria-selected={state.center === 'tools'} className={css.button}
                  onClick={() => { model.center('tools') }}>{t('ctf')}</button>
                <IconAction label={t('ctfClose')} onClick={() => { setToolsOpen(false); model.center('editor') }}>
                  <IconCloseOutlineRegular size={12} />
                </IconAction>
              </div>}
            </div>
            {editing && <div className={css.editorActions}>
              {active.dirty && <Button size="sm" onClick={() => { run(model.save()) }}>{t('ideSave')}</Button>}
              <Menu open={menu === 'editor'} onClose={() => { setMenu(undefined) }} portal align="end" compact
                anchor={<IconAction label={t('ideEditorActions')} expanded={menu === 'editor'} onClick={() => { setMenu(menu === 'editor' ? undefined : 'editor') }}>
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
          {state.error && (
            <div className={`${css.notice} ${css.error}`} role="alert">
              {state.error}
            </div>
          )}
          {active?.comparison?.kind === 'snippet' && activePath !== null && (
            <div className={css.notice}>
              <span>
                {t('ideCompareSnippet')}: {active.comparison.target}
              </span>
              <button
                type="button"
                className={css.button}
                onClick={() => {
                  run(
                    model.applySnippet(activePath).then((result) => {
                      if (result !== 'applied')
                        model.fail(new Error(t(result === 'changed' ? 'ideSnippetChanged' : 'ideSnippetMissing')))
                    }),
                  )
                }}
              >
                {t('ideApplySnippet')}
              </button>
            </div>
          )}
          {state.recoveryConflict && (
            <div className={`${css.notice} ${css.error}`} role="alert">
              {t('ideRecoveryConflict')}
            </div>
          )}
          {active?.external && (
            <div className={css.notice} role="status">
              <span>{t('ideExternal')}</span>
              <button
                type="button"
                className={css.button}
                onClick={() => {
                  run(reloadFile())
                }}
              >
                {t('ideReload')}
              </button>
              {active.comparison?.kind === 'conflict' && (
                <button
                  type="button"
                  className={css.button}
                  onClick={() => {
                    if (activePath !== null) model.acceptConflict(activePath)
                  }}
                >
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
              t={t}
              sendSelection={sendSelection}
              attach={(value) => {
                editor.current = value
              }}
              breakpoint={(path, line) => {
                run(Promise.resolve(execution.toggleBreakpoint(path, line)))
              }}
              stopped={stopped}
              empty={state.workspace === null
                ? <Welcome state={state} t={t} nativeDirectory={nativeDirectory} pending={directoryPending}
                  open={() => { openDirectory('open') }} browse={() => { action('wsl') }}
                  select={(workspace) => { run(model.selectWorkspace(workspace)) }} />
                : <NoFile t={t} shortcut={quickOpenKeys} search={() => { model.quickOpen(true) }} create={() => { run(create(false)) }}
                  agent={layout.agentVisible ? undefined : () => { model.layout({ agentVisible: true }) }} />}
            />
          </div>
          <div className={css.centerBody} hidden={state.center !== 'tools'}>
            {renderSlot('rainy.ide.tools', {
              width: centerWidth,
              close: () => {
                model.center('editor')
              },
            })}
          </div>
        </div>
        <div
          className={css.bottom}
          style={{
            height: layout.bottomVisible ? bottomHeight : 0,
            minHeight: layout.bottomVisible ? undefined : 0,
          }}
          hidden={!layout.bottomVisible}
        >
          {resize('bottom')}
          <IdeBottom
            state={state}
            executionState={executionState}
            execution={execution}
            model={model}
            t={t}
            appearance={appearance}
            reveal={(path, line, column) => {
              run(model.reveal(path, line, column))
            }}
          />
        </div>
      </main>
      <aside className={`${css.pane} ${css.agent}`} hidden={!layout.agentVisible} aria-label={t('ideAgent')}>
        <div className={css.paneHeader}>
          <span className={css.paneTitle}>{t('ideAgent')}</span>
          <IconAction label={t('ideNewChat')} onClick={newChat}><IconNewChatOutlineRegular size={16} /></IconAction>
          <IconAction label={t('ideHistory')} onClick={() => { action('history') }}><IconClockOutlineRegular size={16} /></IconAction>
          <IconAction label={t('ideClose')} onClick={() => { model.layout({ agentVisible: false }) }}><IconCloseOutlineRegular size={14} /></IconAction>
        </div>
        <div className={css.retained} data-rainy-agent>
          {renderFactorySlot('layout.region', {
            region: 'conversation',
            width: rightWidth || 400,
            viewportWidth: width,
            collapsed: false,
            canShow: true,
          })}
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
        {caret !== undefined && caret.text.length > 0
          && <span className={css.statusItem}>{t('ideStatusSelected', { count: caret.text.length })}</span>}
        {active !== undefined && <>
          <span className={css.statusItem}>{sourceLanguage(active.document.path)}</span>
          <span className={css.statusItem}>{active.document.eol.toUpperCase()}</span>
          <span className={css.statusItem}>{t('ideEncoding')}{active.document.bom ? ` ${t('ideBom')}` : ''}</span>
        </>}
      </footer>
      <div className={css.auxiliary} aria-hidden>
        {renderFactorySlot('layout.region', {
          region: 'auxiliary',
          width: 0,
          viewportWidth: width,
          collapsed: true,
          canShow: false,
        })}
      </div>
      {leftVisible && !narrow && resize('left', leftWidth)}
      {layout.agentVisible && resize('right', width - rightWidth)}
      <PromptDialog prompt={prompt} close={resolvePrompt} t={t} />
      <QuickOpenDialog model={model} state={state} t={t} />
      {renderFactorySlot('workspace.directoryBrowser', {
        open: directoryOpen,
        busy: directoryBusy,
        onPicked: adoptDirectory,
        onCancel: () => { setDirectoryOpen(false) },
        onError: (message) => { setDirectoryOpen(false); model.fail(new Error(message)) },
      })}
      <RunConfigurationDialog
        open={runConfiguration}
        close={() => {
          setRunConfiguration(false)
        }}
        state={state}
        model={model}
        t={t}
      />
    </div>
  )
}
