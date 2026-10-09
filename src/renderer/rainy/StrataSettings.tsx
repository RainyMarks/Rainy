/** Explicit controls for bundled local inference and externally selected model weights. */
import { useEffect, useRef, useState } from 'react'
import { Button, IconChevronDownOutlineRegular, IconLoadingOutlineRegular } from '@deepseek-ai/dsh-client-ui-primitives'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import { strataSettingsSchema } from '../strata-protocol.ts'
import type { StrataModelPicker, StrataSettings as EngineSettings, StrataStatus } from '../strata-protocol.ts'
import type { StrataActions, StrataSnapshot } from './strata-controller.ts'
import { Choice } from './Choice.tsx'
import { OptionalModules } from './OptionalModules.tsx'
import css from './SettingsSections.module.css'

interface Props extends StrataActions { snapshot: StrataSnapshot; t: TranslateNS<'rainy'> }
interface Form {
  modelPath: string
  mtpPath: string
  contextWindow: string
  port: string
  kvCache: EngineSettings['kvCache']
  vramReserveMiB: string
  residentBudgetGiB: string
}
const phaseKeys = {
  unconfigured: 'strataUnconfigured', stopped: 'strataStoppedState', preparing: 'strataPreparing', starting: 'strataStarting',
  running: 'strataRunning', stopping: 'strataStopping', external: 'strataExternal', error: 'strataError',
} as const

function formOf(settings: EngineSettings): Form {
  return { modelPath: settings.modelPath, mtpPath: settings.mtpPath, contextWindow: String(settings.contextWindow),
    port: String(settings.port), kvCache: settings.kvCache, vramReserveMiB: String(settings.vramReserveMiB),
    residentBudgetGiB: settings.residentBudgetGiB === null ? '' : String(settings.residentBudgetGiB) }
}

function sameForm(left: Form, right: Form): boolean { return JSON.stringify(left) === JSON.stringify(right) }

/** Show runtime readiness and model controls without creating a server or changing the selected model.
 * The block starts collapsed below the everyday model settings; its summary keeps the engine phase visible.
 * @param props Retained native state, explicit callbacks, and locale-owned text.
 * @returns A collapsible card inside the existing model settings page.
 */
export function StrataSettings(props: Props) {
  const { snapshot, t, strataRefresh } = props
  const [open, setOpen] = useState(false)
  const phase = snapshot.status?.phase
  return <details className={`${css.card} ${css.collapsible}`} data-rainy-strata open={open}
    onToggle={(event) => { setOpen(event.currentTarget.open) }} aria-busy={snapshot.pending !== undefined || snapshot.loading}>
    <summary>
      <span className={css.subheading}>{t('strataTitle')}</span>
      <span className={css.summaryState}>
        {phase !== undefined && <span role="status" aria-live="polite">{t(phaseKeys[phase])}</span>}
        <IconChevronDownOutlineRegular className={css.chevron} size={14} />
      </span>
    </summary>
    <div className={css.stack}>
      {!snapshot.available ? <p className={css.notice}>{t('strataDesktopOnly')}</p> : <>
        {snapshot.error !== '' && <p className={`${css.notice} ${css.error}`} role="alert">{snapshot.error}</p>}
        {snapshot.status === undefined ? <>
          {snapshot.loading ? <div className={css.spinner} role="status" aria-label={t('strataLoading')}><IconLoadingOutlineRegular size={20} /></div>
            : <div className={css.actions}><Button variant="outline" onClick={() => { void strataRefresh() }}>{t('settingsRetry')}</Button></div>}
        </> : <StrataForm {...props} status={snapshot.status} />}
      </>}
    </div>
  </details>
}

function StrataForm({ snapshot, status, t, strataSave, strataStart, strataStop, strataChoose, strataConnect, strataRefresh }:
  Props & { status: StrataStatus }) {
  const [form, setForm] = useState(() => formOf(status.settings))
  const saved = useRef(formOf(status.settings))
  useEffect(() => {
    const previous = saved.current
    const next = formOf(status.settings)
    if (sameForm(previous, next)) return
    saved.current = next
    setForm(current => sameForm(current, previous) ? next : current)
  }, [status.settings])
  const dirty = !sameForm(form, formOf(status.settings))
  const pending = snapshot.pending !== undefined
  const preparing = status.phase === 'preparing' || status.phase === 'starting' || snapshot.pending === 'start'
  const active = preparing || status.phase === 'running' || status.phase === 'stopping'
  const locked = pending || active
  const settings = strataSettingsSchema.safeParse({
    modelPath: form.modelPath.trim(), mtpPath: form.mtpPath.trim(),
    contextWindow: Number(form.contextWindow), port: form.port.trim() === '' ? NaN : Number(form.port),
    kvCache: form.kvCache, vramReserveMiB: form.vramReserveMiB.trim() === '' ? NaN : Number(form.vramReserveMiB),
    residentBudgetGiB: form.residentBudgetGiB.trim() === '' ? null : Number(form.residentBudgetGiB),
  })
  const choose = async (kind: StrataModelPicker, field: 'modelPath' | 'mtpPath'): Promise<void> => {
    const path = await strataChoose(kind)
    if (path !== null) setForm(current => ({ ...current, [field]: path }))
  }
  const input = (key: 'modelPath' | 'mtpPath' | 'port' | 'vramReserveMiB' | 'residentBudgetGiB', label: string,
    range?: { min: number; max?: number; step?: number | string }) =>
    <label className={css.field}><span>{label}</span><input className={css.input} aria-label={label} disabled={locked}
      type={range === undefined ? 'text' : 'number'} {...range} value={form[key]}
      onChange={(event) => { setForm(current => ({ ...current, [key]: event.target.value })) }} /></label>
  const contexts = ['8192', '32768', '65536', '131072', '262144']
  if (!contexts.includes(form.contextWindow)) contexts.push(form.contextWindow)
  const canStop = preparing || (status.server?.owned === true && status.phase === 'running')
  return <>
    <div className={css.row}>
      <p className={status.runtime.available ? css.muted : `${css.notice} ${css.error}`}>
        {t(status.runtime.available ? 'strataRuntimeReady' : 'strataRuntimeMissing')}
        {status.runtime.available && status.runtime.version !== null ? ` · ${status.runtime.version}` : ''}
      </p>
      <Button size="sm" variant="outline" disabled={snapshot.loading || pending} onClick={() => { void strataRefresh() }}>{t('settingsRefresh')}</Button>
    </div>
    {!status.runtime.available && <OptionalModules t={t} only="strata" onChange={() => { void strataRefresh() }} />}
    <p className={css.muted}>{t('strataSupported')}</p>
    {input('modelPath', t('strataModelPath'))}
    <div className={css.actions}>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('gguf', 'modelPath') }}>{t('strataChooseGguf')}</Button>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('directory', 'modelPath') }}>{t('strataChooseDirectory')}</Button>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('profile', 'modelPath') }}>{t('strataChooseProfile')}</Button>
    </div>
    {status.profiles.length > 0 && <label className={css.field}><span>{t('strataProfiles')}</span>
      <Choice label={t('strataProfiles')} value={status.profiles.some(profile => profile.path === form.modelPath) ? form.modelPath : ''}
        disabled={locked} items={[{ id: '', label: t('strataProfileNone') }, ...status.profiles.map(profile => ({ id: profile.path, label: profile.label }))]}
        onChange={(path) => { if (path !== '') setForm(current => ({ ...current, modelPath: path })) }} /></label>}
    {input('mtpPath', t('strataMtpPath'))}
    <div className={css.actions}>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('mtp', 'mtpPath') }}>{t('strataChooseMtpFile')}</Button>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('directory', 'mtpPath') }}>{t('strataChooseMtp')}</Button>
    </div>
    <div className={css.grid}>
      <label className={css.field}><span>{t('strataContext')}</span><Choice label={t('strataContext')} value={form.contextWindow}
        disabled={locked} items={contexts.map(value => ({ id: value, label: Number(value).toLocaleString() }))}
        onChange={(value) => { setForm(current => ({ ...current, contextWindow: value })) }} /></label>
      {input('port', t('strataPort'), { min: 1024, max: 65535, step: 1 })}
    </div>
    <details><summary>{t('strataAdvanced')}</summary><div className={`${css.grid} ${css.disclosed}`}>
      <label className={css.field}><span>{t('strataKvCache')}</span><Choice label={t('strataKvCache')} value={form.kvCache}
        disabled={locked} items={(['int8', 'q4_0', 'k8v4'] as const).map(value => ({ id: value, label: value }))}
        onChange={(value) => {
          const kvCache = strataSettingsSchema.shape.kvCache.parse(value)
          setForm(current => ({ ...current, kvCache }))
        }} /></label>
      {input('vramReserveMiB', t('strataVramReserve'), { min: 0, step: 1 })}
      {input('residentBudgetGiB', t('strataResidentBudget'), { min: 0, step: 'any' })}
    </div></details>
    {dirty && <p className={css.muted}>{t('strataUnsaved')}</p>}
    {!settings.success && <p className={`${css.notice} ${css.error}`} role="alert">{t('strataInvalid')}</p>}
    {status.model?.needsPreparation && <p className={css.muted}>{t('strataPreparingNote')}</p>}
    {status.progress !== null && <p className={css.notice} role="status" aria-live="polite">{status.progress}</p>}
    {status.error !== null && <p className={`${css.notice} ${css.error}`} role="alert">{status.error}</p>}
    {status.server?.loaded && <p className={css.muted}>{t('strataHealth', { model: status.server.model, context: status.server.contextWindow.toLocaleString() })}</p>}
    {status.server?.owned === false && <p className={css.muted}>{t('strataExternalNote')}</p>}
    {status.server?.authenticationRequired && <p className={css.notice}>{t('strataAuthentication')}</p>}
    <div className={css.actions}>
      <Button variant="outline" disabled={locked || !dirty || !settings.success} onClick={() => {
        if (settings.success) void strataSave(settings.data).then((value) => { if (value !== undefined) setForm(formOf(value.settings)) })
      }}>{t('strataSave')}</Button>
      <Button variant="outline" disabled={locked || dirty || form.modelPath.trim() === '' || !status.runtime.available || status.phase === 'external'}
        onClick={() => { void strataStart() }}>{t('strataStart')}</Button>
      <Button variant="outline" disabled={!canStop || (pending && snapshot.pending !== 'start')}
        onClick={() => { void strataStop() }}>{t(preparing ? 'strataCancelStart' : 'strataStop')}</Button>
      <Button variant="primary" disabled={pending || dirty || !status.server?.loaded || status.server.authenticationRequired || (status.phase !== 'running' && status.phase !== 'external')}
        onClick={() => { void strataConnect() }}>{t('strataConnect')}</Button>
    </div>
  </>
}
