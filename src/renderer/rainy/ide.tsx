/** Rainy IDE composition and bridges to existing workspace, session, and settings services. */
import type { Context } from '@deepseek-ai/cordis'
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import type {} from '@deepseek-ai/dsh-client-ui-sidebar-files/client'
import type {} from '@deepseek-ai/dsh-client-ui-workspace/client'
import type {} from '@deepseek-ai/dsh-client-ui-conversation/client'
import type { HostObservable } from '@deepseek-ai/dsh-client-ui-slots'
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import type { Config } from '../config.ts'
import type { EditorAppearance } from './editor-types.ts'
import { createIdeFilesApi } from './ide-api.ts'
import { IdeModel } from './ide-model.ts'
import { IdeDirectorySelection } from './ide-directory-selection.ts'
import { IdeShell } from './IdeShell.tsx'
import type { DesktopWorkbenchBridge } from './ctf-protocol.ts'
import type { IdeWorkspace } from '../ide-files-protocol.ts'
import type { IdeExecutionModel } from './ide-execution-model.ts'
import { createIdeExecutionApi } from './ide-execution-api.ts'
import { IdeExecutionModel as ExecutionModel } from './ide-execution-model.ts'
import { IdeRequestError } from './ide-api.ts'
import { absoluteFilePath, fileLabel } from './ide-paths.ts'

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface SlotMap {
    /** Retained central tool surface owned by the Rainy workspace. */
    'rainy.ide.tools': { kind: 'single'; scope: 'root'; owner: { readonly width: number; readonly close: () => void } }
  }
}

/** Stable empty snapshot while no quick-open binding exists. */
const NO_KEYS: readonly string[] = []

/** Private model and service callbacks exposed to the workspace through framework injection. */
export interface IdeShellInjected {
  readonly model: IdeModel
  readonly execution: IdeExecutionModel
  readonly hooks: {
    readonly ide: HostObservable<ReturnType<IdeModel['state']['getSnapshot']>>
    readonly appearance: HostObservable<EditorAppearance>
    readonly execution: HostObservable<ReturnType<IdeExecutionModel['state']['getSnapshot']>>
    /** Effective key labels, including platform separators, for quick open. */
    readonly quickOpenShortcut: HostObservable<readonly string[]>
    readonly directoryPending: HostObservable<boolean>
  }
  readonly openFolder: (mode?: 'open' | 'attach') => Promise<void>
  readonly nativeDirectory: boolean
  readonly newChat: () => void
  readonly settings: () => void
  readonly sendSelection: () => Promise<void>
}

interface NativeIdeBridge {
  selectDirectory(): Promise<{ path: string; displayPath: string } | null>
}

/** Install independent editor state while preserving the existing conversation renderer.
 * @param ctx Rainy feature context.
 * @param config Validated observation and persistence timings.
 * @returns The retained workspace model for sibling Rainy contributions.
 */
export function installIde(ctx: Context, config: Config): IdeModel {
  const t = ctx.locale.bind('rainy')
  const appearance = createSnapshotStore<EditorAppearance>({
    dark: ctx.theme.getTheme().active.colorScheme === 'dark',
    fontSize: ctx.theme.getTheme().codeFontSize,
  })
  const model = new IdeModel(createIdeFilesApi(), {
    debounceMs: config.editorStateDebounceMs,
    pollMs: config.editorPollMs,
    isSessionSelected: sessionId => ctx.uiSession.adapter.current.getSnapshot().key === sessionId,
    describeError: (error) => {
      if (error instanceof IdeRequestError) {
        const messages: Record<string, () => string> = {
          'not-found': () => t('ideMissingPath'), 'invalid-path': () => t('ideInvalidPath'),
          'outside-workspace': () => t('ideOutsideRoot'), 'workspace-unavailable': () => t('ideUnavailableRoot'),
          'unsaved-root': () => t('ideUnsavedRoot'),
          'root-overlap': () => t('ideRootOverlap'),
        }
        const describe = messages[error.code]
        if (describe !== undefined) return describe()
      }
      return error instanceof Error ? error.message : String(error)
    },
    restoreSession: (workspace, sessionId) => {
      const saved = sessionId === null ? undefined : ctx.sessions.list.getSnapshot().byId[sessionId]
      if (sessionId !== null && saved?.cwd === workspace.path)
        ctx.uiWorkspace.openSession(sessionId)
      else ctx.uiWorkspace.clearSession()
      return Promise.resolve()
    },
  })
  const execution = new ExecutionModel(createIdeExecutionApi(), {
    pollMs: config.editorPollMs,
    activePollMs: config.executionPollMs,
    maxOutputCharacters: config.editorMaxOutputCharacters,
    maxRetainedWorkspaces: config.editorMaxRetainedWorkspaces,
    terminalCols: config.editorTerminalCols,
    terminalRows: config.editorTerminalRows,
    getConfiguration: () =>
      model.state.getSnapshot().data.execution ?? { profiles: [], activeProfile: null, breakpoints: [], watches: [] },
    setConfiguration: (value) => {
      model.execution(value)
    },
    onError: (error) => {
      model.fail(error)
    },
    onReveal: (path, line, column) => {
      void model.reveal(path, line, column).catch((error: unknown) => {
        model.fail(error)
      })
    },
  })
  const directorySelection = new IdeDirectorySelection({
    choose: async () => {
      const native = (window as typeof window & { __RAINY_IDE_NATIVE__?: NativeIdeBridge }).__RAINY_IDE_NATIVE__
      return (await native?.selectDirectory())?.path ?? null
    },
    adopt: (path, mode) => mode === 'attach' ? model.attachRoot(path) : model.openWorkspace(path),
  })
  ctx.on('workspace-files/resolve-open', () => () => { model.quickOpen(true) })
  const newChat = (): void => {
    ctx.uiWorkspace.clearSession()
    model.layout({ agentVisible: true })
  }
  const settings = (): void => {
    window.dispatchEvent(
      new CustomEvent('rainy:open-panel', {
        detail: { page: 'settings', sessionId: ctx.uiSession.adapter.current.getSnapshot().key },
      }),
    )
  }
  const sendSelection = async (): Promise<void> => {
    const state = model.state.getSnapshot()
    const selection = state.selection
    if (selection === undefined || state.workspace === null || selection.text === '') return
    const workspace = state.workspace
    const selected = ctx.uiSession.adapter.current.getSnapshot().key as SessionId | undefined
    const existing = selected === undefined ? undefined : ctx.sessions.list.getSnapshot().byId[selected]
    const sessionId =
      existing?.cwd === workspace.path ? selected : await ctx.uiWorkspace.connectWorkspace(workspace.workspaceId)
    if (sessionId === undefined) return
    ctx.uiWorkspace.openSession(sessionId)
    await ctx.sessions.using(sessionId, { source: 'controllerOperation' }, async (reference) => {
      await reference.ready
      const scope = ctx.sessions.scope(sessionId)
      const conversation = scope?.get('conversation')
      if (conversation === undefined) throw new Error(t('needsSession'))
      await conversation.send(
        t('ideSelectionMessage', {
          path: absoluteFilePath(workspace, selection.path) ?? fileLabel(workspace, selection.path),
          range: `${selection.startLine}:${selection.startColumn}–${selection.endLine}:${selection.endColumn}`,
          language: selection.language,
          text: selection.text,
        }),
      )
    })
    model.layout({ agentVisible: true })
  }
  ctx.slots.inject('shell.workspace', () =>
    ctx.slots.register(
      {
        name: 'shell.workspace',
        locale: 'rainy',
        children: { 'rainy.ide.tools': { kind: 'single', scope: 'root' } },
        inject: (): IdeShellInjected => ({
          model,
          execution,
          hooks: { ide: model.state, appearance, execution: execution.state, directoryPending: directorySelection.pending,
            quickOpenShortcut: { subscribe: listener => ctx.shortcuts.catalog.subscribe(listener),
              getSnapshot: () => ctx.shortcuts.catalog.getSnapshot().find(entry => entry.id === 'workspace.files')?.keys ?? NO_KEYS } },
          openFolder: mode => directorySelection.open(mode),
          nativeDirectory: (window as typeof window & { __RAINY_IDE_NATIVE__?: NativeIdeBridge }).__RAINY_IDE_NATIVE__ !== undefined,
          newChat,
          settings,
          sendSelection,
        }),
      },
      IdeShell,
    ),
  )
  ctx.effect(() => {
    const syncAppearance = (): void => {
      const theme = ctx.theme.getTheme()
      appearance.set({ dark: theme.active.colorScheme === 'dark', fontSize: theme.codeFontSize })
    }
    let workspace: IdeWorkspace | null = null
    const syncWorkspace = (): void => {
      const next = model.state.getSnapshot().workspace
      if (workspace?.workspaceId === next?.workspaceId) return
      workspace = next
      execution.setWorkspace(next?.workspaceId ?? null)
    }
    let historyNavigation: SessionId | undefined
    const syncSession = (): void => {
      const sessionId = ctx.uiSession.adapter.current.getSnapshot().key as SessionId | undefined
      const selected = sessionId === undefined ? undefined : ctx.sessions.list.getSnapshot().byId[sessionId]
      if (model.state.getSnapshot().phase !== 'ready') return
      if (sessionId === undefined) model.session(null)
      else if (selected?.cwd === model.state.getSnapshot().workspace?.path) model.session(sessionId)
      else if (selected?.cwd !== undefined && historyNavigation === undefined) {
        historyNavigation = sessionId
        void model.openWorkspace(selected.cwd, sessionId).catch((error: unknown) => { model.fail(error) }).finally(() => {
          historyNavigation = undefined
          if (ctx.uiSession.adapter.current.getSnapshot().key !== sessionId) syncSession()
        })
      }
    }
    const disposers = [
      ctx.on('theme/change', syncAppearance),
      model.state.subscribe(syncWorkspace),
      ctx.uiSession.adapter.current.subscribe(syncSession),
      ctx.sessions.list.subscribe(syncSession),
    ]
    const desktop = (window as typeof window & { __RAINY_WORKBENCH__?: DesktopWorkbenchBridge }).__RAINY_WORKBENCH__
    if (desktop !== undefined)
      disposers.push(desktop.onFlush(async () => ({ ok: await model.flush(), error: model.state.getSnapshot().error })))
    void model.initialize()
    return async () => {
      directorySelection.dispose()
      for (const dispose of disposers) dispose()
      await execution.dispose()
      model.dispose()
    }
  }, 'rainy: workspace editor')
  return model
}
