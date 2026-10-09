/** Composition of the IDE models, the CTF workbench and the chat shown beside them. */
import { DEFAULT_CONFIG, type Config } from '../../shared/config.ts'
import type { IdeWorkspace } from '../../shared/ide-files-protocol.ts'
import type { NativeToolsBridge } from '../../shared/native-tools-protocol.ts'
import type { SessionSummary } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { createStore, type Store } from '../ui/store.ts'
import { createIdeFilesApi, IdeRequestError, type IdeFilesApi } from '../ide/ide-api.ts'
import { createIdeExecutionApi, type IdeExecutionApi } from '../ide/ide-execution-api.ts'
import { IdeModel } from '../ide/ide-model.ts'
import { IdeExecutionModel } from '../ide/ide-execution-model.ts'
import { IdeDirectorySelection } from '../ide/ide-directory-selection.ts'
import { absoluteFilePath, fileKey, fileLabel, keyFromAbsolute } from '../ide/ide-paths.ts'
import { ideMessages, type IdeMessageKey } from '../ide/messages.ts'
import { createCtfWorkbench, type CtfWorkbench } from '../ctf/ctf-workbench.ts'
import type { DesktopWorkbenchBridge } from '../ctf/ctf-protocol.ts'
import { emit, on, type WorkbenchEvents } from './bus.ts'

/** Native folder picker exposed by the Electron preload as `__RAINY_IDE_NATIVE__`. */
export interface NativeIdeBridge {
  selectDirectory(): Promise<{ readonly path: string; readonly displayPath: string } | null>
}

/** Construction inputs; every bridge is optional so the page also runs in a plain browser and in tests. */
export interface WorkbenchOptions {
  readonly config?: Config | undefined
  readonly files?: IdeFilesApi | undefined
  readonly execution?: IdeExecutionApi | undefined
  readonly native?: NativeIdeBridge | undefined
  readonly tools?: NativeToolsBridge | undefined
  readonly desktop?: DesktopWorkbenchBridge | undefined
  /** Whether a remembered chat belongs to a project; defaults to the Host's chat list of that project. */
  readonly sessionInWorkspace?: ((sessionId: string, workspace: IdeWorkspace) => Promise<boolean>) | undefined
}

/** Long-lived models behind the window layout. */
export interface Workbench {
  readonly model: IdeModel
  readonly execution: IdeExecutionModel
  readonly directory: IdeDirectorySelection
  readonly ctf: CtfWorkbench
  /** Chat shown in the AI pane; `null` shows the new-chat composer. */
  readonly session: Store<string | null>
  /** Whether the desktop's native folder picker is available. */
  readonly nativeDirectory: boolean
  /** Load the project list and restore the last project. */
  start(): void
  /**
   * Show a chat of the current project (or the new-chat composer) and remember it for the project.
   * @param sessionId Chat to show, or `null` for a new chat.
   */
  showSession(sessionId: string | null): void
  /**
   * Show a chat picked from the history, opening its project first when it belongs to another one.
   * @param summary Picked chat.
   * @returns Completion of the project switch.
   */
  openSession(summary: SessionSummary): Promise<void>
  /** Put the editor selection into the AI composer and show the AI pane. */
  sendSelection(): void
  /** @returns Completion after outstanding requests settle; running processes stay with the Host. */
  dispose(): Promise<void>
}

const errorMessages: Readonly<Record<string, IdeMessageKey>> = {
  'not-found': 'ideMissingPath', 'invalid-path': 'ideInvalidPath', 'outside-workspace': 'ideOutsideRoot',
  'workspace-unavailable': 'ideUnavailableRoot', 'unsaved-root': 'ideUnsavedRoot', 'root-overlap': 'ideRootOverlap',
}

/**
 * Describe an IDE failure in the interface language.
 * @param error Operation failure.
 * @returns Localized text for known IDE error codes, else the error message.
 */
export function describeIdeError(error: unknown): string {
  if (error instanceof IdeRequestError) {
    const key = Object.hasOwn(errorMessages, error.code) ? errorMessages[error.code] : undefined
    if (key !== undefined) return ideMessages.t(key)
  }
  return error instanceof Error ? error.message : String(error)
}

async function hostSessionInWorkspace(sessionId: string, workspace: IdeWorkspace): Promise<boolean> {
  try {
    const sessions = await host.call('sessions.list', { workspaceId: workspace.workspaceId })
    return sessions.some(session => session.id === sessionId)
  } catch (_unavailable) {
    // An unreadable chat list restores no chat; the project itself still opens.
    return false
  }
}

/**
 * Construct the workbench. Nothing is requested from the Host until {@link Workbench.start}.
 * @param options Bridges and test substitutes.
 * @returns The workbench.
 */
export function createWorkbench(options: WorkbenchOptions = {}): Workbench {
  const config = options.config ?? DEFAULT_CONFIG
  const sessionInWorkspace = options.sessionInWorkspace ?? hostSessionInWorkspace
  const session = createStore<string | null>(null)
  let pending: SessionSummary | undefined
  let historySelection: string | undefined
  let navigating = false
  const model = new IdeModel(options.files ?? createIdeFilesApi(), {
    debounceMs: config.editorStateDebounceMs,
    pollMs: config.editorPollMs,
    describeError: describeIdeError,
    isSessionSelected: sessionId => pending === undefined && historySelection === sessionId,
    restoreSession: async (workspace, sessionId) => {
      if (sessionId === null) session.set(null)
      else if (sessionId === historySelection || await sessionInWorkspace(sessionId, workspace)) session.set(sessionId)
      else session.set(null)
    },
  })
  const execution = new IdeExecutionModel(options.execution ?? createIdeExecutionApi(), {
    pollMs: config.editorPollMs,
    activePollMs: config.executionPollMs,
    maxOutputCharacters: config.editorMaxOutputCharacters,
    maxRetainedWorkspaces: config.editorMaxRetainedWorkspaces,
    terminalCols: config.editorTerminalCols,
    terminalRows: config.editorTerminalRows,
    getConfiguration: () =>
      model.state.getSnapshot().data.execution ?? { profiles: [], activeProfile: null, breakpoints: [], watches: [] },
    setConfiguration: (value) => { model.execution(value) },
    onError: (error) => { model.fail(error) },
    onReveal: (path, line, column) => {
      void model.reveal(path, line, column).catch((error: unknown) => { model.fail(error) })
    },
  })
  const native = options.native
  const directory = new IdeDirectorySelection({
    choose: async () => (await native?.selectDirectory())?.path ?? null,
    adopt: (path, mode) => mode === 'attach' ? model.attachRoot(path) : model.openWorkspace(path),
  })
  const ctf = createCtfWorkbench(config, { tools: options.tools, desktop: options.desktop })

  const showSession = (sessionId: string | null): void => {
    session.set(sessionId)
    model.session(sessionId)
  }
  const navigate = async (summary: SessionSummary): Promise<void> => {
    const current = model.state.getSnapshot()
    const workspace = current.workspace
    if (summary.workspaceId === null || summary.workspaceId === workspace?.workspaceId) {
      if (summary.workspaceId === null) session.set(summary.id)
      else showSession(summary.id)
      model.layout({ agentVisible: true })
      return
    }
    const target = current.workspaces.find(entry => entry.workspaceId === summary.workspaceId)
    if (target !== undefined) await model.selectWorkspace(target, summary.id)
    else await model.openWorkspace(summary.cwd, summary.id)
  }
  const openSession = async (summary: SessionSummary): Promise<void> => {
    pending = summary
    if (navigating) return
    navigating = true
    try {
      while (pending !== undefined) {
        const next: SessionSummary = pending
        pending = undefined
        historySelection = next.id
        await navigate(next).catch((error: unknown) => { model.fail(error) })
      }
    } finally {
      navigating = false
      historySelection = undefined
    }
  }
  const sendSelection = (): void => {
    const state = model.state.getSnapshot()
    const selection = state.selection
    if (selection === undefined || state.workspace === null || selection.text === '') return
    const workspace = state.workspace
    model.layout({ agentVisible: true })
    emit('chat.send', {
      workspaceId: workspace.workspaceId,
      text: ideMessages.t('ideSelectionMessage', {
        path: absoluteFilePath(workspace, selection.path) ?? fileLabel(workspace, selection.path),
        range: `${selection.startLine}:${selection.startColumn}–${selection.endLine}:${selection.endColumn}`,
        language: selection.language,
        text: selection.text,
      }),
    })
  }
  const openPath = async ({ path, workspaceId, rootId, line }: WorkbenchEvents['editor.open']): Promise<void> => {
    let state = model.state.getSnapshot()
    if (workspaceId !== undefined && state.workspace?.workspaceId !== workspaceId) {
      const target = state.workspaces.find(entry => entry.workspaceId === workspaceId)
      if (target === undefined) throw new Error(ideMessages.t('ideNoWorkspace'))
      await model.selectWorkspace(target)
      state = model.state.getSnapshot()
      if (state.workspace?.workspaceId !== workspaceId) return
    }
    const workspace = state.workspace
    if (workspace === null) throw new Error(ideMessages.t('ideNoWorkspace'))
    const absolute = path.startsWith('/') || path.startsWith('\\\\') || /^[A-Za-z]:[\\/]/u.test(path)
    const key = absolute ? keyFromAbsolute(workspace, path) : fileKey(path.replaceAll('\\', '/').replace(/^\.\//u, ''), rootId)
    if (key === undefined) throw new Error(ideMessages.t('ideOutsideRoot'))
    if (line === undefined) await model.openFile(key)
    else await model.reveal(key, line, 1)
  }

  let workspaceId = model.state.getSnapshot().workspace?.workspaceId ?? null
  const disposers = [
    model.state.subscribe(() => {
      const next = model.state.getSnapshot().workspace?.workspaceId ?? null
      if (next === workspaceId) return
      workspaceId = next
      execution.setWorkspace(next)
    }),
    on('editor.open', (event) => { void openPath(event).catch((error: unknown) => { model.fail(error) }) }),
    on('editor.snippet', ({ code, language, compare }) => {
      model.openSnippet(code, language ?? '', ideMessages.t('ideSnippet'), compare)
    }),
  ]
  const unregister = options.desktop?.onFlush(async () => {
    const ok = await model.flush()
    return ok ? { ok } : { ok, error: model.state.getSnapshot().error }
  })
  if (unregister !== undefined) disposers.push(unregister)
  let started = false
  return {
    model,
    execution,
    directory,
    ctf,
    session,
    nativeDirectory: native !== undefined,
    start: () => {
      if (started) return
      started = true
      void model.initialize()
    },
    showSession,
    openSession,
    sendSelection,
    dispose: async () => {
      directory.dispose()
      for (const dispose of disposers.splice(0)) dispose()
      ctf.dispose()
      await execution.dispose()
      model.dispose()
    },
  }
}

let shared: Workbench | undefined

/** @returns The page's workbench, created on first use from the Host configuration and the desktop bridges. */
export function sharedWorkbench(): Workbench {
  shared ??= createWorkbench({
    config: window.__RAINY_WORKBENCH_CONFIG__,
    native: window.__RAINY_IDE_NATIVE__,
    tools: window.__RAINY_TOOLS__,
    desktop: window.__RAINY_WORKBENCH__,
  })
  return shared
}
