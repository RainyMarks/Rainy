/** Network proxy for model requests: follow the system, connect directly, or use a typed address; with a connection test. */
import { useEffect, useState } from 'react'
import type { ProxySettings as ProxyChoice, ProxyStatus, ProxyTestResult } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { Button } from '../ui/Button.tsx'
import { useT } from './messages.ts'
import { ActionFailure, Notice, Select, SettingRow, errorText, type SettingsAction } from './parts.tsx'
import css from './sections.module.css'

/**
 * @param props.action The Models section's operation runner.
 * @returns A card with the proxy mode, the manual address, the address in effect and a test button.
 */
export function ProxySettings({ action }: { action: SettingsAction }): JSX.Element {
  const t = useT()
  const [status, setStatus] = useState<ProxyStatus | undefined>()
  const [mode, setMode] = useState<ProxyChoice['mode']>('system')
  const [url, setUrl] = useState('')
  const [error, setError] = useState('')
  const [test, setTest] = useState<ProxyTestResult | undefined>()
  const [testing, setTesting] = useState(false)
  useEffect(() => {
    host.call('network.status').then((value) => {
      setStatus(value); setMode(value.settings.mode); setUrl(value.settings.url ?? '')
    }, (failure: unknown) => { setError(errorText(failure)) })
    // The address in effect is the one for the default model's service, so it is read again when the models change.
    return host.on('models.changed', () => { host.call('network.status').then(setStatus, () => undefined) })
  }, [])
  const choice: ProxyChoice = mode === 'manual' ? { mode, url } : { mode }
  const changed = status !== undefined && (status.settings.mode !== mode || (mode === 'manual' && (status.settings.url ?? '') !== url.trim()))
  const runTest = (): void => {
    setTesting(true); setTest(undefined)
    host.call('network.test', choice).then(setTest, (failure: unknown) => { setTest({ ok: false, url: '', ms: 0, error: errorText(failure) }) })
      .finally(() => { setTesting(false) })
  }
  const modes = [{ id: 'system' as const, label: t('proxySystem') }, { id: 'off' as const, label: t('proxyOff') }, { id: 'manual' as const, label: t('proxyManual') }]
  return <article className={css.card} data-proxy-settings>
    <h3 className={css.subheading}>{t('proxyTitle')}</h3>
    <p className={css.muted}>{t('proxyNote')}</p>
    {error !== '' && <Notice tone="error">{error}</Notice>}
    <div className={css.rows}>
      <SettingRow title={t('proxyMode')} description={status === undefined ? undefined
        : status.effective === null ? t('proxyDirect') : t('proxyInEffect', { url: status.effective })}>
        <Select label={t('proxyMode')} value={mode} items={modes} disabled={status === undefined || action.busy} onChange={(value) => { setMode(value); setTest(undefined) }} />
      </SettingRow>
    </div>
    {mode === 'manual' && <input className={css.input} aria-label={t('proxyAddress')} placeholder="127.0.0.1:7890" value={url} spellCheck={false}
      onChange={(event) => { setUrl(event.target.value); setTest(undefined) }} />}
    {mode === 'system' && status !== undefined && <p className={css.muted}>{status.system === null ? t('proxySystemNone') : t('proxySystemFound', { url: status.system })}</p>}
    <div className={css.row}>
      <Button variant="outline" disabled={status === undefined || testing || (mode === 'manual' && url.trim() === '')} onClick={runTest}>
        {testing ? t('proxyTesting') : t('proxyTest')}
      </Button>
      <Button variant="primary" disabled={!changed || action.busy || (mode === 'manual' && url.trim() === '')} onClick={() => {
        action.run(async () => { const next = await host.call('network.set', choice); setStatus(next); setUrl(next.settings.url ?? '') }, t('settingsSaved'), 'proxy')
      }}>{t('settingsSave')}</Button>
    </div>
    <ActionFailure action={action} place="proxy" />
    {test !== undefined && <Notice tone={test.ok ? 'status' : 'error'}>
      {test.ok ? t('proxyTestOk', { url: test.url, status: test.status ?? 0, ms: test.ms }) : t('proxyTestFailed', { error: test.error ?? '' })}
    </Notice>}
  </article>
}
