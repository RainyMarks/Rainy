/** Download and remove optional desktop components. */
import { useEffect, useRef, useState } from 'react'
import type { OptionalModuleId, OptionalModuleStatus, OptionalModulesState } from '../../shared/modules-protocol.ts'
import { Button } from '../ui/Button.tsx'
import { useT } from './messages.ts'
import { modulesBridge } from './native.ts'
import { Notice, errorText, fileSizeText } from './parts.tsx'
import css from './sections.module.css'

const copy = {
  strata: { name: 'moduleStrata', hint: 'moduleStrataHint' },
  php: { name: 'modulePhp', hint: 'modulePhpHint' },
} as const

/**
 * List components with their sizes and offer download, cancellation and removal.
 * @param props.only Show one component inline, for example inside the Strata card.
 * @param props.onChange Called after a component was installed or removed.
 * @returns The component list, or nothing outside the desktop app.
 */
export function OptionalModules({ only, onChange }: { only?: OptionalModuleId; onChange?: () => void }): JSX.Element | null {
  const t = useT()
  const bridge = modulesBridge()
  const [modules, setModules] = useState<readonly OptionalModuleStatus[]>([])
  const [progress, setProgress] = useState<OptionalModulesState>({ phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' })
  const [busy, setBusy] = useState(false)
  const changed = useRef(onChange)
  changed.current = onChange
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    if (bridge === undefined) return undefined
    const refresh = (): void => {
      void bridge.list().then((value) => { if (mounted.current) setModules(value) }, (error: unknown) => {
        if (mounted.current) setProgress({ phase: 'error', completedBytes: 0, totalBytes: 0, error: errorText(error) })
      })
    }
    refresh()
    void bridge.state().then((value) => { if (mounted.current) setProgress(value) }, () => undefined)
    const stop = bridge.onProgress((value) => {
      if (!mounted.current) return
      setProgress(value)
      if (value.phase === 'complete') { refresh(); changed.current?.() }
    })
    return () => { mounted.current = false; stop() }
  }, [bridge])
  if (bridge === undefined) return null
  const running = progress.phase === 'downloading' || progress.phase === 'installing'
  const act = (operation: () => Promise<void>): void => {
    setBusy(true)
    void operation().then(async () => {
      const value = await bridge.list()
      if (mounted.current) setModules(value)
      changed.current?.()
    }, (error: unknown) => {
      if (mounted.current) setProgress(previous => ({ ...previous, phase: 'error', error: errorText(error) }))
    }).finally(() => { if (mounted.current) setBusy(false) })
  }
  const shown = modules.filter(module => only === undefined || module.id === only)
  return <div className={css.stack} data-optional-modules>
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
          {active ? t(progress.phase === 'installing' ? 'modulesInstalling' : 'modulesProgress', { progress: percentage })
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
      && <Notice tone="error">{progress.error}</Notice>}
  </div>
}
