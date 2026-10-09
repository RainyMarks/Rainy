/** Download and remove optional desktop components from settings. */
import { useEffect, useRef, useState } from 'react'
import { Button, fileSizeText } from '@deepseek-ai/dsh-client-ui-primitives'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import type { OptionalModuleId, OptionalModuleStatus, OptionalModulesBridge, OptionalModulesState } from '../modules-protocol.ts'
import css from './SettingsSections.module.css'

const copy = {
  strata: { name: 'moduleStrata', hint: 'moduleStrataHint' },
  php: { name: 'modulePhp', hint: 'modulePhpHint' },
} as const

/** @returns the desktop bridge, absent in a browser. */
function modulesBridge(): OptionalModulesBridge | undefined {
  return (globalThis as typeof globalThis & { __RAINY_MODULES__?: OptionalModulesBridge }).__RAINY_MODULES__
}

/**
 * List components with their sizes and offer download, cancellation and removal.
 * @param props.t - Rainy locale.
 * @param props.only - show one component inline, for example inside the Strata settings.
 * @param props.onChange - called after a component was installed or removed.
 * @returns the component list, or nothing outside the desktop app.
 */
export function OptionalModules({ t, only, onChange }: { t: TranslateNS<'rainy'>; only?: OptionalModuleId; onChange?: () => void }) {
  const bridge = modulesBridge()
  const [modules, setModules] = useState<readonly OptionalModuleStatus[]>([])
  const [progress, setProgress] = useState<OptionalModulesState>({ phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' })
  const [busy, setBusy] = useState(false)
  const changed = useRef(onChange)
  changed.current = onChange
  useEffect(() => {
    if (bridge === undefined) return undefined
    let active = true
    const refresh = (): void => {
      void bridge.list().then((value) => { if (active) setModules(value) }, (error: unknown) => {
        if (active) setProgress({ phase: 'error', completedBytes: 0, totalBytes: 0, error: error instanceof Error ? error.message : String(error) })
      })
    }
    refresh()
    void bridge.state().then((value) => { if (active) setProgress(value) })
    const stop = bridge.onProgress((value) => {
      if (!active) return
      setProgress(value)
      if (value.phase === 'complete') { refresh(); changed.current?.() }
    })
    return () => { active = false; stop() }
  }, [bridge])
  if (bridge === undefined) return null
  const running = progress.phase === 'downloading' || progress.phase === 'installing'
  const act = (operation: () => Promise<void>): void => {
    setBusy(true)
    void operation().then(async () => { setModules(await bridge.list()); changed.current?.() }, (error: unknown) => {
      setProgress({ ...progress, phase: 'error', error: error instanceof Error ? error.message : String(error) })
    }).finally(() => { setBusy(false) })
  }
  const shown = modules.filter(module => only === undefined || module.id === only)
  return <div className={css.stack} data-rainy-modules>
    {only === undefined && <>
      <h3 className={css.subheading}>{t('modulesTitle')}</h3>
      <p className={css.muted}>{t('modulesNote')}</p>
    </>}
    {shown.map((module) => {
      const active = running && progress.module === module.id
      const percentage = progress.totalBytes > 0 ? Math.floor(progress.completedBytes * 100 / progress.totalBytes) : 0
      return <div key={module.id} className={css.row} data-module={module.id}>
        <p className={css.muted}>
          <strong>{t(copy[module.id].name)}</strong> · {t(copy[module.id].hint)}<br />
          {active ? t(progress.phase === 'installing' ? 'modulesInstalling' : 'modulesProgress', { progress: String(percentage) })
            : module.installed ? t('modulesInstalled', { size: fileSizeText(module.unpackedBytes) }) : t('modulesNotInstalled')}
        </p>
        <div className={css.actions}>
          {active ? <Button size="sm" variant="outline" onClick={() => { void bridge.cancel() }}>{t('modulesCancel')}</Button>
            : module.installed ? <Button size="sm" variant="outline" disabled={busy || running}
              onClick={() => { act(() => bridge.remove(module.id)) }}>{t('modulesRemove')}</Button>
              : <Button size="sm" variant="primary" disabled={busy || running}
                onClick={() => { act(() => bridge.install(module.id)) }}>{t('modulesDownload', { size: fileSizeText(module.downloadBytes) })}</Button>}
        </div>
      </div>
    })}
    {progress.phase === 'cancelled' && (only === undefined || progress.module === only) && <p className={css.muted}>{t('modulesCancelled')}</p>}
    {progress.phase === 'error' && (only === undefined || progress.module === only || progress.module === undefined)
      && <p className={`${css.notice} ${css.error}`} role="alert">{progress.error}</p>}
  </div>
}
