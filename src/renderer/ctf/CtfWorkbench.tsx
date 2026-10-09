/** Central human tool workbench with isolated draft contexts. */
import type { Context } from '@deepseek-ai/cordis'
import type { SidebarRightApplicationId } from '@deepseek-ai/dsh-client-ui-sidebar-right/client'
import type {} from '@deepseek-ai/dsh-client-ui-theme/client'
import type {} from '@deepseek-ai/dsh-client-ui-session/client'
import type {} from '@deepseek-ai/dsh-api-session-controller/client'
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import type { HostObservable, InjectFace, PropsLocale, PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import { IconLoadingOutlineRegular, Button, Toast, SegmentedTabs } from '@deepseek-ai/dsh-client-ui-primitives'
import type { Config } from '../config.ts'
import type { DesktopWorkbenchBridge } from './ctf-protocol.ts'
import { CtfWorkbenchBridge, type CtfWorkbenchState } from './ctf-bridge.ts'
import { readCtfColors } from './ctf-colors.ts'
import type { NativeToolsBridge } from '../native-tools-protocol.ts'
import { NativeToolsController, type NativeToolsState } from './native-tools.ts'
import { ToolCatalog, type ToolCatalogActions } from './ToolCatalog.tsx'
import css from './CtfWorkbench.module.css'
import { useEffect, useState } from 'react'
import type { IdeModel } from './ide-model.ts'
import type {} from './ide.tsx'

/** Registered root workspace identity. */
export const CTF_WORKBENCH_ID = '@deepseek-ai/dsh-client-ui-rainy/ctf' as SidebarRightApplicationId

type WorkbenchView = 'catalog' | 'icesky'
interface WorkbenchInjected extends ToolCatalogActions {
  readonly hooks: {
    readonly workbench: HostObservable<CtfWorkbenchState>
    readonly tools: HostObservable<NativeToolsState>
    readonly view: HostObservable<WorkbenchView>
  }
  readonly selectView: (view: WorkbenchView) => void
  readonly attach: (frame: HTMLIFrameElement | null) => void
  readonly retry: () => Promise<boolean>
  readonly loadFailed: () => void
}

/**
 * Retained central tool surface; logical draft ownership arrives through the frame protocol.
 * @param props - host geometry, copy, and bridge state.
 * @returns the retained workbench.
 */
export function CtfWorkbench({ width, t, useWorkbench, useTools, useView, selectView,
  loadTools, launchTool, toggleFavorite, operateTools, cancelDownload, checkToolUpdates, attach, retry, loadFailed }:
  PropsRuntime<'rainy.ide.tools'> & PropsLocale<'rainy'> & InjectFace<WorkbenchInjected>) {
  const state = useWorkbench(value => value)
  const tools = useTools(value => value)
  const view = useView(value => value)
  const [visitedIceSky, setVisitedIceSky] = useState(false)
  useEffect(() => { if (view === 'icesky') setVisitedIceSky(true) }, [view])
  useEffect(() => { void loadTools() }, [loadTools])
  const error = state.phase === 'error' || state.saving === 'error'
    ? t(state.error === 'save' || state.saving === 'error' ? 'ctfSaveFailed' : 'ctfLoadFailed') : undefined
  return <section className={css.root} style={{ width }} data-rainy-ctf-workbench>
    <header className={css.header}>
      <SegmentedTabs label={t('toolsView')} value={view} onChange={selectView} items={[
        { value: 'catalog', label: t('toolsCatalog'), id: 'rainy-tools-tab', panelId: 'rainy-tools-panel' },
        { value: 'icesky', label: t('toolsIceSky'), id: 'rainy-icesky-tab', panelId: 'rainy-icesky-panel' },
      ]} />
      {state.saving !== 'idle' && <span className={css.status} role="status">
        {t(state.saving === 'saving' ? 'ctfSaving' : state.saving === 'saved' ? 'ctfSaved' : 'ctfUnsaved')}
      </span>}
    </header>
    <div className={css.body}>
      <div id="rainy-tools-panel" role="tabpanel" aria-labelledby="rainy-tools-tab" className={css.panel} hidden={view !== 'catalog'}>
        <ToolCatalog t={t} state={tools} loadTools={loadTools} launchTool={launchTool} toggleFavorite={toggleFavorite}
          operateTools={operateTools} cancelDownload={cancelDownload} checkToolUpdates={checkToolUpdates} />
      </div>
      <div id="rainy-icesky-panel" role="tabpanel" aria-labelledby="rainy-icesky-tab" className={css.panel} hidden={view !== 'icesky'}>
        {(view === 'icesky' || visitedIceSky) && <iframe ref={attach} className={css.frame} src="/rainy/icesky/index.html?embed=rainy"
          title={t('ctfFrameTitle')} loading="lazy" referrerPolicy="no-referrer" onError={loadFailed} />}
        {state.phase === 'loading' && <div className={css.loading} role="status" aria-label={t('ctfLoading')}><IconLoadingOutlineRegular className={css.spinner} /></div>}
        {error !== undefined && <div className={css.error} role="alert"><span>{error}</span>
          {state.message && <small>{state.message}</small>}
          <Button size="sm" onClick={() => { void retry() }}>{t('ctfRetry')}</Button>
        </div>}
      </div>
    </div>
  </section>
}

interface Notice { readonly id: number; readonly message: string; readonly kind: 'success' | 'error' | 'warning' }
interface ToastInjected { readonly hooks: { readonly notice: HostObservable<Notice | undefined> }; readonly dismiss: () => void }
function WorkbenchToast({ useNotice, dismiss }: PropsRuntime<'shell.overlay'> & InjectFace<ToastInjected>) {
  const notice = useNotice(value => value)
  return notice === undefined ? null : <Toast key={notice.id} text={notice.message} {...notice.kind === 'success' ? { tone: 'success' as const } : {}} onDone={dismiss} />
}

function LegacyCtfTab({ useTabInfo, open }: PropsRuntime<'sidebar.right.pane.tab'> & { readonly open: () => void }) {
  const { tab } = useTabInfo()
  useEffect(() => {
    if (!tab.visible) return
    open()
    tab.actions.close()
  }, [tab.visible, tab.actions, open])
  return null
}

/**
 * Register one lazily visited body with retained feedback and desktop save owners.
 * @param ctx - Rainy Client context.
 * @param config - validated deadlines.
 */
export function installCtfWorkbench(ctx: Context, config: Config, editor: IdeModel): void {
  const t = ctx.locale.bind('rainy')
  const notice = createSnapshotStore<Notice | undefined>(undefined)
  const view = createSnapshotStore<WorkbenchView>('catalog')
  let noticeId = 0
  const toast = (message: string, kind: 'success' | 'error' | 'warning'): void => { notice.set({ id: ++noticeId, message, kind }) }
  const bridge = new CtfWorkbenchBridge({ origin: location.origin,
    readyTimeoutMs: config.readyTimeoutMs, flushTimeoutMs: config.flushTimeoutMs,
    toast, flushFailureMessage: () => t('ctfSaveFailed') })
  const native = (globalThis as typeof globalThis & { __RAINY_TOOLS__?: NativeToolsBridge }).__RAINY_TOOLS__
  const tools = new NativeToolsController(native, { opened: name => t('toolsOpened', { name }),
    launchFailed: name => t('toolsLaunchFailed', { name }), favoritesFailed: () => t('toolsFavoritesFailed'),
    favoriteSaved: selected => t(selected ? 'toolsFavoriteSaved' : 'toolsFavoriteRemoved'),
    completed: operation => t(({ install: 'toolsDoneInstall', update: 'toolsDoneUpdate', remove: 'toolsDoneRemove', repair: 'toolsDoneRepair' } as const)[operation]),
    downloadFailed: () => t('toolsDownloadFailed') }, toast)
  const loadTools = (): Promise<void> => tools.load()
  const launchTool: ToolCatalogActions['launchTool'] = (id, variant) => tools.launch(id, variant)
  const toggleFavorite: ToolCatalogActions['toggleFavorite'] = id => tools.toggleFavorite(id)
  const operateTools: ToolCatalogActions['operateTools'] = (operation, ids) => tools.operate(operation, ids)
  const cancelDownload = (): Promise<void> => tools.cancelDownload()
  const checkToolUpdates = (): Promise<void> => tools.checkUpdates()
  const selectView = (next: WorkbenchView): void => { view.set(next) }
  const attach = (frame: HTMLIFrameElement | null): void => { bridge.attach(frame) }
  const retry = (): Promise<boolean> => bridge.retry()
  const loadFailed = (): void => { bridge.loadFailed() }
  const open = (): void => { selectView('icesky'); editor.center('tools') }
  ctx.effect(() => ctx.sidebarRightApplications.register(CTF_WORKBENCH_ID,
    { refresh: () => { if (view.getSnapshot() === 'catalog') void loadTools(); else void bridge.retry() } }), 'rainy: CTF workspace')
  ctx.effect(() => {
    const receive = (event: MessageEvent<unknown>): void => { bridge.receive(event) }
    window.addEventListener('message', receive)
    let disposed = false
    let scheduled = false
    const configure = (): void => {
      const sessionId = ctx.uiSession.adapter.current.getSnapshot().key as SessionId | undefined
      const standalone = sessionId === undefined || ctx.sessions.list.getSnapshot().byId[sessionId]?.blank === true
      const appearance = ctx.theme.getTheme()
      const colors = readCtfColors(document)
      bridge.configure({ context: standalone ? { kind: 'standalone' } : { kind: 'session', id: sessionId },
        appearance: { dark: appearance.active.colorScheme === 'dark', fontSize: appearance.fontSize,
          codeFontSize: appearance.codeFontSize, locale: ctx.locale.getSnapshot().active === 'zh' ? 'zh' : 'en',
          ...colors === undefined ? {} : { colors } },
        visible: view.getSnapshot() === 'icesky' && editor.state.getSnapshot().center === 'tools' })
    }
    const sync = (): void => {
      if (scheduled || disposed) return
      scheduled = true
      queueMicrotask(() => {
        scheduled = false
        if (!disposed) configure()
      })
    }
    const disposers = [ctx.uiSession.adapter.current.subscribe(sync), ctx.sessions.list.subscribe(sync),
      editor.state.subscribe(sync), view.subscribe(sync),
      ctx.on('theme/change', sync), ctx.on('locale/change', sync)]
    sync()
    const desktop = (globalThis as typeof globalThis & { __RAINY_WORKBENCH__?: DesktopWorkbenchBridge }).__RAINY_WORKBENCH__
    if (desktop !== undefined) disposers.push(desktop.onFlush(() => bridge.flush()))
    return () => {
      disposed = true
      for (const dispose of disposers) dispose()
      window.removeEventListener('message', receive)
      bridge.dispose()
      tools.dispose()
    }
  }, 'rainy: CTF frame bridge')
  ctx.slots.inject('rainy.ide.tools', () => ctx.slots.register({ name: 'rainy.ide.tools', locale: 'rainy',
    inject: (): WorkbenchInjected => ({ hooks: { workbench: bridge.state, tools: tools.state, view },
      attach, retry, loadFailed, selectView, loadTools, launchTool, toggleFavorite, operateTools, cancelDownload, checkToolUpdates }),
  }, CtfWorkbench))
  ctx.effect(() => ctx.sidebarRightTabs.register({ id: CTF_WORKBENCH_ID, kind: 'rainy-ctf', title: () => t('ctf') }), 'rainy: previous CTF navigation')
  ctx.slots.inject('sidebar.right.pane.tab', () => ctx.slots.register({ name: 'sidebar.right.pane.tab', key: CTF_WORKBENCH_ID,
    inject: () => ({ open }),
  }, LegacyCtfTab))
  ctx.slots.inject('shell.overlay', () => ctx.slots.register({ name: 'shell.overlay', id: `${CTF_WORKBENCH_ID}/toast`,
    inject: (): ToastInjected => ({ hooks: { notice }, dismiss: () => { notice.set(undefined) } }),
  }, WorkbenchToast))
}
