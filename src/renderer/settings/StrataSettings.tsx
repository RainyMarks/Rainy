/** Controls for the bundled local inference engine and user-selected model weights. */
import { useEffect, useRef, useState } from 'react'
import clsx from 'clsx'
import { strataSettingsSchema } from '../../shared/strata-protocol.ts'
import type { StrataModelPicker, StrataSettings as EngineSettings, StrataStatus } from '../../shared/strata-protocol.ts'
import { Button } from '../ui/Button.tsx'
import { IconChevronDownOutlineRegular } from '../ui/icons/index.tsx'
import { useT } from './messages.ts'
import { OptionalModules } from './OptionalModules.tsx'
import { Field, Loading, Notice, Select } from './parts.tsx'
import type { StrataActions, StrataSnapshot } from './strata-controller.ts'
import css from './sections.module.css'

interface Props { snapshot: StrataSnapshot; actions: StrataActions }
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
const kvCaches = ['int8', 'q4_0', 'k8v4'] as const

function formOf(settings: EngineSettings): Form {
  return { modelPath: settings.modelPath, mtpPath: settings.mtpPath, contextWindow: String(settings.contextWindow),
    port: String(settings.port), kvCache: settings.kvCache, vramReserveMiB: String(settings.vramReserveMiB),
    residentBudgetGiB: settings.residentBudgetGiB === null ? '' : String(settings.residentBudgetGiB) }
}

function sameForm(left: Form, right: Form): boolean { return JSON.stringify(left) === JSON.stringify(right) }

/**
 * Show engine readiness and model controls without starting a server or changing the selected model.
 * The card starts collapsed below the everyday model settings; its summary keeps the engine phase visible.
 * @param props Controller snapshot and operations.
 * @returns A collapsible card.
 */
export function StrataSettings(props: Props): JSX.Element {
  const { snapshot, actions } = props
  const t = useT()
  const [open, setOpen] = useState(false)
  const phase = snapshot.status?.phase
  return <details className={clsx(css.card, css.collapsible)} data-strata open={open}
    onToggle={(event) => { setOpen(event.currentTarget.open) }} aria-busy={snapshot.pending !== undefined || snapshot.loading}>
    <summary>
      <span className={css.subheading}>{t('strataTitle')}</span>
      <span className={css.summaryState}>
        {phase !== undefined && <span role="status" aria-live="polite">{t(phaseKeys[phase])}</span>}
        <IconChevronDownOutlineRegular className={css.chevron} size={14} />
      </span>
    </summary>
    <div className={css.stack}>
      {!snapshot.available ? <Notice>{t('strataDesktopOnly')}</Notice> : <>
        {snapshot.error !== '' && <Notice tone="error">{snapshot.error}</Notice>}
        {snapshot.status === undefined
          ? snapshot.loading ? <Loading label={t('strataLoading')} />
            : <div className={css.actions}><Button variant="outline" onClick={() => { void actions.strataRefresh() }}>{t('settingsRetry')}</Button></div>
          : <StrataForm {...props} status={snapshot.status} />}
      </>}
    </div>
  </details>
}

function StrataForm({ snapshot, actions, status }: Props & { status: StrataStatus }): JSX.Element {
  const t = useT()
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
    const path = await actions.strataChoose(kind)
    if (path !== null) setForm(current => ({ ...current, [field]: path }))
  }
  const input = (key: 'modelPath' | 'mtpPath' | 'port' | 'vramReserveMiB' | 'residentBudgetGiB', label: string,
    range?: { min: number; max?: number; step?: number | string }) =>
    <Field label={label}><input className={css.input} aria-label={label} disabled={locked}
      type={range === undefined ? 'text' : 'number'} {...range} value={form[key]}
      onChange={(event) => { const value = event.target.value; setForm(current => ({ ...current, [key]: value })) }} /></Field>
  const contexts = ['8192', '32768', '65536', '131072', '262144']
  if (!contexts.includes(form.contextWindow)) contexts.push(form.contextWindow)
  const canStop = preparing || (status.server?.owned === true && status.phase === 'running')
  return <>
    <div className={css.row}>
      <p className={status.runtime.available ? css.muted : clsx(css.notice, css.error)}>
        {t(status.runtime.available ? 'strataRuntimeReady' : 'strataRuntimeMissing')}
        {status.runtime.available && status.runtime.version !== null ? ` · ${status.runtime.version}` : ''}
      </p>
      <Button size="sm" variant="outline" disabled={snapshot.loading || pending} onClick={() => { void actions.strataRefresh() }}>{t('settingsRefresh')}</Button>
    </div>
    {!status.runtime.available && <OptionalModules only="strata" onChange={() => { void actions.strataRefresh() }} />}
    <p className={css.muted}>{t('strataSupported')}</p>
    {input('modelPath', t('strataModelPath'))}
    <div className={css.actions}>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('gguf', 'modelPath') }}>{t('strataChooseGguf')}</Button>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('directory', 'modelPath') }}>{t('strataChooseDirectory')}</Button>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('profile', 'modelPath') }}>{t('strataChooseProfile')}</Button>
    </div>
    {status.profiles.length > 0 && <Field label={t('strataProfiles')}>
      <Select label={t('strataProfiles')} value={status.profiles.some(profile => profile.path === form.modelPath) ? form.modelPath : ''}
        disabled={locked} items={[{ id: '', label: t('strataProfileNone') }, ...status.profiles.map(profile => ({ id: profile.path, label: profile.label }))]}
        onChange={(path) => { if (path !== '') setForm(current => ({ ...current, modelPath: path })) }} /></Field>}
    {input('mtpPath', t('strataMtpPath'))}
    <div className={css.actions}>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('mtp', 'mtpPath') }}>{t('strataChooseMtpFile')}</Button>
      <Button variant="outline" disabled={locked} onClick={() => { void choose('directory', 'mtpPath') }}>{t('strataChooseMtp')}</Button>
    </div>
    <div className={css.grid}>
      <Field label={t('strataContext')}><Select label={t('strataContext')} value={form.contextWindow}
        disabled={locked} items={contexts.map(value => ({ id: value, label: Number(value).toLocaleString('en-US') }))}
        onChange={(value) => { setForm(current => ({ ...current, contextWindow: value })) }} /></Field>
      {input('port', t('strataPort'), { min: 1024, max: 65535, step: 1 })}
    </div>
    <details className={css.disclosure}><summary>{t('strataAdvanced')}</summary><div className={clsx(css.grid, css.disclosed)}>
      <Field label={t('strataKvCache')}><Select label={t('strataKvCache')} value={form.kvCache}
        disabled={locked} items={kvCaches.map(value => ({ id: value, label: value }))}
        onChange={(kvCache) => { setForm(current => ({ ...current, kvCache })) }} /></Field>
      {input('vramReserveMiB', t('strataVramReserve'), { min: 0, step: 1 })}
      {input('residentBudgetGiB', t('strataResidentBudget'), { min: 0, step: 'any' })}
    </div></details>
    {dirty && <p className={css.muted}>{t('strataUnsaved')}</p>}
    {!settings.success && <Notice tone="error">{t('strataInvalid')}</Notice>}
    {status.model?.needsPreparation === true && <p className={css.muted}>{t('strataPreparingNote')}</p>}
    {status.progress !== null && <Notice tone="status">{status.progress}</Notice>}
    {status.error !== null && <Notice tone="error">{status.error}</Notice>}
    {status.server?.loaded === true && <p className={css.muted}>{t('strataHealth', { model: status.server.model, context: status.server.contextWindow.toLocaleString('en-US') })}</p>}
    {status.server?.owned === false && <p className={css.muted}>{t('strataExternalNote')}</p>}
    {status.server?.authenticationRequired === true && <Notice>{t('strataAuthentication')}</Notice>}
    <div className={css.actions}>
      <Button variant="outline" disabled={locked || !dirty || !settings.success} onClick={() => {
        if (settings.success) void actions.strataSave(settings.data).then((value) => { if (value !== undefined) setForm(formOf(value.settings)) })
      }}>{t('strataSave')}</Button>
      <Button variant="outline" disabled={locked || dirty || form.modelPath.trim() === '' || !status.runtime.available || status.phase === 'external'}
        onClick={() => { void actions.strataStart() }}>{t('strataStart')}</Button>
      <Button variant="outline" disabled={!canStop || (pending && snapshot.pending !== 'start')}
        onClick={() => { void actions.strataStop() }}>{t(preparing ? 'strataCancelStart' : 'strataStop')}</Button>
      <Button variant="primary" disabled={pending || dirty || status.server?.loaded !== true || status.server.authenticationRequired
        || (status.phase !== 'running' && status.phase !== 'external')}
        onClick={() => { void actions.strataConnect() }}>{t('strataConnect')}</Button>
    </div>
  </>
}
