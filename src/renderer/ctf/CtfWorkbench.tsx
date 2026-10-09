/** CTF workbench tab: the common tool catalog and the IceSky frame, kept mounted once visited. */
import { useCallback, useEffect, useState } from 'react'
import { Button, IconLoadingOutlineRegular, SegmentedTabs, useStore } from '../ui/index.ts'
import { usePrefs } from '../prefs.ts'
import { ToolCatalog } from './ToolCatalog.tsx'
import type { CtfView, CtfWorkbench as CtfWorkbenchOwner } from './ctf-workbench.ts'
import { useCtfT } from './messages.ts'
import css from './CtfWorkbench.module.css'

/**
 * Render the workbench.
 * @param props.owner Frame bridge, tool catalog and selected view.
 * @param props.sessionId Chat shown in the AI pane; IceSky keeps separate drafts per chat.
 * @param props.active Whether the workbench tab is the visible centre surface.
 * @param props.dark Whether the page uses the dark palette.
 * @returns The workbench.
 */
export function CtfWorkbench({ owner, sessionId, active, dark }: {
  owner: CtfWorkbenchOwner
  sessionId: string | null
  active: boolean
  dark: boolean
}) {
  const t = useCtfT()
  const prefs = usePrefs()
  const state = useStore(owner.bridge.state)
  const tools = useStore(owner.tools.state)
  const view = useStore(owner.view)
  const [visitedIceSky, setVisitedIceSky] = useState(view === 'icesky')
  useEffect(() => { if (view === 'icesky') setVisitedIceSky(true) }, [view])
  useEffect(() => { void owner.tools.load() }, [owner])
  const visible = active && view === 'icesky'
  useEffect(() => {
    owner.configure({ sessionId, visible,
      appearance: { dark, fontSize: prefs.uiFontSize, codeFontSize: prefs.codeFontSize, locale: prefs.locale } })
  }, [owner, sessionId, visible, dark, prefs.uiFontSize, prefs.codeFontSize, prefs.locale])
  const selectView = (next: CtfView): void => { owner.view.set(next) }
  const attach = useCallback((frame: HTMLIFrameElement | null) => { owner.bridge.attach(frame) }, [owner])
  const error = state.phase === 'error' || state.saving === 'error'
    ? t(state.error === 'save' || state.saving === 'error' ? 'ctfSaveFailed' : 'ctfLoadFailed') : undefined
  return <section className={css.root} data-rainy-ctf-workbench>
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
        <ToolCatalog state={tools} loadTools={() => owner.tools.load()} launchTool={(id, variant) => owner.tools.launch(id, variant)}
          toggleFavorite={id => owner.tools.toggleFavorite(id)} operateTools={(operation, ids) => owner.tools.operate(operation, ids)}
          cancelDownload={() => owner.tools.cancelDownload()} checkToolUpdates={() => owner.tools.checkUpdates()} />
      </div>
      <div id="rainy-icesky-panel" role="tabpanel" aria-labelledby="rainy-icesky-tab" className={css.panel} hidden={view !== 'icesky'}>
        {visitedIceSky && <iframe ref={attach} className={css.frame} src="/rainy/icesky/index.html?embed=rainy"
          title={t('ctfFrameTitle')} referrerPolicy="no-referrer" onError={() => { owner.bridge.loadFailed() }} />}
        {visitedIceSky && state.phase === 'loading' && <div className={css.loading} role="status" aria-label={t('ctfLoading')}>
          <IconLoadingOutlineRegular className={css.spinner} />
        </div>}
        {error !== undefined && <div className={css.error} role="alert"><span>{error}</span>
          {state.message && <small>{state.message}</small>}
          <Button size="sm" onClick={() => { void owner.bridge.retry() }}>{t('ctfRetry')}</Button>
        </div>}
      </div>
    </div>
  </section>
}
