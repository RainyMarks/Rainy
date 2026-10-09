/** Project environments and bounded memory in the shared settings dialog. */
import { useEffect, useRef, useState } from 'react'
import { Button } from '@deepseek-ai/dsh-client-ui-primitives'
import type { RuntimeLanguage, RuntimeSnapshot } from '../runtime-protocol.ts'
import type { ProjectMemoryStatus, RuntimeNativeHost } from './settings-protocol.ts'
import type { SettingsSectionProps } from './SettingsSections.tsx'
import { SettingsField, SettingsLoading, useSettingsAction } from './SettingsSections.tsx'
import { Choice } from './Choice.tsx'
import { OptionalModules } from './OptionalModules.tsx'
import css from './SettingsSections.module.css'

const languages: readonly RuntimeLanguage[] = ['python', 'node', 'php', 'c', 'cpp']

/** Discover and select existing interpreters without changing installed packages.
 * @param props Host environment operations and selected project source.
 * @returns The runtime settings section.
 */
export function RuntimeSection({ useIde, runtime, runtimeNative, notify, t }: SettingsSectionProps) {
  const workspaceId = useIde(value => value.workspace?.workspaceId)
  const currentProject = useRef(workspaceId)
  currentProject.current = workspaceId
  const [snapshot, setSnapshot] = useState<RuntimeSnapshot | undefined>()
  const [targets, setTargets] = useState<Awaited<ReturnType<RuntimeNativeHost['targets']>> | undefined>()
  const [language, setLanguage] = useState<RuntimeLanguage>('python')
  const [manualPath, setManualPath] = useState('')
  const [error, setError] = useState('')
  const [preparing, setPreparing] = useState(false)
  const [preparationMessage, setPreparationMessage] = useState('')
  const mounted = useRef(true)
  const { busy, run } = useSettingsAction(notify)
  useEffect(() => {
    mounted.current = true
    const stop = runtimeNative?.onProgress((message) => { if (mounted.current) setPreparationMessage(message) })
    return () => { mounted.current = false; stop?.() }
  }, [runtimeNative])
  useEffect(() => {
    let cancelled = false
    setSnapshot(undefined); setError('')
    if (workspaceId !== undefined) void runtime({ op: 'status', workspaceId }).then((result) => { if (!cancelled) setSnapshot(result) })
      .catch((failure: unknown) => { if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure)) })
    if (runtimeNative !== undefined) void runtimeNative.targets().then((result) => { if (!cancelled) setTargets(result) })
      .catch((failure: unknown) => { if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure)) })
    return () => { cancelled = true }
  }, [workspaceId, runtime, runtimeNative])
  const accept = (value: RuntimeSnapshot): void => { if (value.workspaceId === currentProject.current) { setSnapshot(value); setError('') } }
  const selected = snapshot?.selected[language]
  const candidates = snapshot?.candidates.filter(candidate => candidate.language === language) ?? []
  const prepare = (): void => {
    if (runtimeNative === undefined) return
    run(async () => {
      setPreparationMessage(''); setPreparing(true)
      try {
        await runtimeNative.prepare()
        const available = await runtimeNative.targets()
        if (mounted.current) setTargets(available)
      } finally { if (mounted.current) setPreparing(false) }
    })
  }
  return <section className={css.section} data-rainy-settings="runtime">
    <h2 className={css.heading}>{t('settingsRuntime')}</h2>
    {targets !== undefined && <SettingsField label={t('settingsExecutionTarget')}><Choice label={t('settingsExecutionTarget')}
      value={targets.current.id} items={targets.targets.map(target => ({ id: target.id, label: target.label }))} disabled={busy}
      onChange={(targetId) => { if (targetId !== targets.current.id && runtimeNative !== undefined) run(async () => {
        const switched = await runtimeNative.switchTarget({ targetId, ...(workspaceId === undefined ? {} : { workspaceId }) })
        if (!switched.ok) throw new Error(switched.error ?? t('settingsMissingCapability'))
      }) }} /></SettingsField>}
    <p className={css.muted}>{t('settingsEnvironmentNote')}</p>
    {error !== '' && <p className={`${css.notice} ${css.error}`} role="alert">{error}</p>}
    {workspaceId === undefined ? <p className={css.notice}>{t('settingsOpenProject')}</p> : <>
      <div className={css.actions}>
        <Button variant="outline" disabled={busy} onClick={() => { run(async () => { accept(await runtime({ op: 'discover', workspaceId })) }) }}>{t('settingsDiscoverEnvironments')}</Button>
        {runtimeNative !== undefined && <Button variant="outline" disabled={busy} onClick={prepare}>{t('settingsPrepareEnvironments')}</Button>}
      </div>
      {preparing && <p className={css.notice} role="status" aria-live="polite">{preparationMessage || t('settingsPreparingEnvironments')}</p>}
      <SettingsField label={t('ideLanguage')}><Choice label={t('ideLanguage')} value={language} items={languages.map(id => ({ id, label: id }))}
        disabled={busy} onChange={(value) => {
          const chosen = languages.find(item => item === value)
          if (chosen !== undefined) setLanguage(chosen)
        }} /></SettingsField>
      <SettingsField label={t('settingsRuntime')}><Choice label={t('settingsRuntime')} value={selected?.path ?? ''}
        items={[{ id: '', label: t('settingsAutomatic') }, ...candidates.map(candidate => ({ id: candidate.path,
          label: `${candidate.path}${candidate.version === null ? '' : ` · ${candidate.version}`}`, disabled: !candidate.ready }))]}
        disabled={busy} onChange={(path) => { run(async () => { accept(await runtime({ op: 'select', workspaceId, language, path: path || null })) }, t('settingsApplied')) }} /></SettingsField>
      <SettingsField label={t('settingsEnvironmentPath')}><input className={css.input} value={manualPath} onChange={(event) => { setManualPath(event.target.value) }} /></SettingsField>
      <div className={css.actions}><Button variant="outline" disabled={busy || manualPath.trim() === ''} onClick={() => { run(async () => {
        accept(await runtime({ op: 'probe', workspaceId, language, path: manualPath.trim() }))
      }) }}>{t('settingsProbeEnvironment')}</Button></div>
      {candidates.map(candidate => <article className={css.card} key={candidate.id}>
        <div className={css.row}><span>{candidate.path}</span><span>{t(candidate.ready ? 'settingsReadyCapability' : 'settingsMissingCapability')}</span></div>
        <p className={css.muted}>{candidate.version ?? candidate.platform}</p>
        {candidate.error !== undefined && <p className={`${css.body} ${css.error}`}>{candidate.error}</p>}
        {candidate.capabilities.length > 0 && <ul className={css.list}>{candidate.capabilities.map(capability => <li key={capability.name}>
          {capability.name} · {t(capability.ready ? 'settingsReadyCapability' : 'settingsMissingCapability')}{capability.detail === undefined ? '' : ` · ${capability.detail}`}
        </li>)}</ul>}
      </article>)}
    </>}
    <OptionalModules t={t} />
  </section>
}

/** Inspect, edit, or remove only the currently selected project's durable memory.
 * @param props Project-scoped memory controls and locale.
 * @returns Separate read and generation switches with revision-checked editing.
 */
export function MemorySection({ useIde, memory, memoryEnabled, memoryEdit, memoryDelete, memoryClear, notify, t }: SettingsSectionProps) {
  const workspaceId = useIde(value => value.workspace?.workspaceId)
  const currentProject = useRef(workspaceId)
  currentProject.current = workspaceId
  const [snapshot, setSnapshot] = useState<ProjectMemoryStatus | undefined>()
  const [error, setError] = useState('')
  const [edit, setEdit] = useState<{ id: string; text: string; revision: number } | undefined>()
  const [clearRequested, setClearRequested] = useState(false)
  const { busy, run } = useSettingsAction(notify)
  const reload = async (): Promise<void> => {
    if (workspaceId === undefined) return
    const value = await memory(workspaceId)
    if (currentProject.current === workspaceId) { setSnapshot(value); setError('') }
  }
  useEffect(() => {
    let cancelled = false
    setSnapshot(undefined); setError(''); setEdit(undefined); setClearRequested(false)
    if (workspaceId !== undefined) void memory(workspaceId).then((value) => { if (!cancelled) setSnapshot(value) })
      .catch((failure: unknown) => { if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure)) })
    return () => { cancelled = true }
  }, [workspaceId, memory])
  return <section className={css.section} data-rainy-settings="memory">
    <h2 className={css.heading}>{t('settingsMemory')}</h2><p className={css.muted}>{t('settingsMemoryNote')}</p>
    {workspaceId === undefined ? <p className={css.notice}>{t('settingsOpenProject')}</p> : <>
      <div className={css.actions}><Button variant="outline" disabled={busy} onClick={() => { run(reload) }}>{t('settingsRefresh')}</Button></div>
      {error !== '' && <p className={`${css.notice} ${css.error}`} role="alert">{error}</p>}
      {snapshot === undefined && error === '' && <SettingsLoading label={t('ideLoading')} />}
      {snapshot !== undefined && <>
        <label className={css.switch}><input type="checkbox" checked={snapshot.enabled} disabled={busy} onChange={(event) => {
          const enabled = event.target.checked; run(async () => { await memoryEnabled(workspaceId, { enabled }); await reload() }, t('settingsApplied'))
        }} />{t('settingsMemoryUse')}</label>
        <label className={css.switch}><input type="checkbox" checked={snapshot.generationEnabled} disabled={busy} onChange={(event) => {
          const generationEnabled = event.target.checked; run(async () => { await memoryEnabled(workspaceId, { generationEnabled }); await reload() }, t('settingsApplied'))
        }} />{t('settingsMemoryGenerate')}</label>
        {snapshot.generating ? <p className={css.notice} role="status">{t('settingsMemoryGenerating')}</p>
          : snapshot.pending ? <p className={css.notice} role="status">{t('settingsMemoryPending')}</p> : null}
        {snapshot.error !== undefined && <p className={`${css.notice} ${css.error}`} role="alert">{snapshot.error}</p>}
        {snapshot.items.length === 0 && <p className={css.notice}>{t('settingsMemoryEmpty')}</p>}
        {snapshot.items.map(item => <article className={css.card} key={item.id}>
          {edit?.id === item.id ? <SettingsField label={t('settingsMemoryText')}><textarea className={css.textarea} value={edit.text}
            onChange={(event) => { setEdit({ ...edit, text: event.target.value }) }} /></SettingsField>
            : <p className={css.body}>{item.text}</p>}
          {item.sources.map((source, index) => <p className={css.muted} key={`${source.sessionId}:${source.seq}:${index}`}>
            {t('settingsMemorySource', { session: source.sessionId, seq: source.seq, target: source.executionTargetId })}
          </p>)}
          <div className={css.actions}>
            {edit?.id === item.id ? <>
              <Button variant="primary" disabled={busy || edit.text.trim() === ''} onClick={() => { run(async () => {
                await memoryEdit(workspaceId, edit.id, edit.text, edit.revision); setEdit(undefined); await reload()
              }, t('settingsSaved')) }}>{t('settingsSave')}</Button>
              <Button disabled={busy} onClick={() => { setEdit(undefined) }}>{t('settingsCancel')}</Button>
            </> : <>
              <Button variant="outline" disabled={busy} onClick={() => { setEdit({ id: item.id, text: item.text, revision: snapshot.revision }) }}>{t('settingsMemoryEdit')}</Button>
              <Button disabled={busy} onClick={() => { run(async () => { await memoryDelete(workspaceId, item.id); await reload() }, t('settingsApplied')) }}>{t('settingsMemoryDelete')}</Button>
            </>}
          </div>
        </article>)}
        {snapshot.items.length > 0 && <div className={css.actions}>
          {clearRequested ? <>
            <Button variant="outline" disabled={busy} onClick={() => { run(async () => {
              await memoryClear(workspaceId); setClearRequested(false); setEdit(undefined); await reload()
            }, t('settingsApplied')) }}>{t('settingsMemoryClearConfirm')}</Button>
            <Button disabled={busy} onClick={() => { setClearRequested(false) }}>{t('settingsCancel')}</Button>
          </> : <Button disabled={busy} onClick={() => { setClearRequested(true) }}>{t('settingsMemoryClear')}</Button>}
        </div>}
      </>}
    </>}
  </section>
}
