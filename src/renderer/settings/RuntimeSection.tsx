/** Runtime section: execution target, interpreters of the current project and optional components. */
import { useEffect, useRef, useState } from 'react'
import clsx from 'clsx'
import type { RuntimeLanguage, RuntimeSnapshot } from '../../shared/runtime-protocol.ts'
import { host } from '../rpc.ts'
import { Button } from '../ui/Button.tsx'
import { useT } from './messages.ts'
import { runtimeBridge } from './native.ts'
import type { RuntimeNativeHost } from './native.ts'
import { OptionalModules } from './OptionalModules.tsx'
import { Field, Loading, Notice, Select, errorText, useAction } from './parts.tsx'
import type { SettingsWorkspace } from './parts.tsx'
import css from './sections.module.css'

const languages: readonly RuntimeLanguage[] = ['python', 'node', 'php', 'c', 'cpp']

/**
 * Switch the execution target, then discover and select existing interpreters without installing anything.
 * @param props.workspace Project whose interpreter selection is shown.
 * @returns The section.
 */
export function RuntimeSection({ workspace }: { workspace: SettingsWorkspace }): JSX.Element {
  const t = useT()
  const native = runtimeBridge()
  const workspaceId = workspace?.workspaceId
  const [snapshot, setSnapshot] = useState<RuntimeSnapshot | undefined>()
  const [targets, setTargets] = useState<Awaited<ReturnType<RuntimeNativeHost['targets']>> | undefined>()
  const [language, setLanguage] = useState<RuntimeLanguage>('python')
  const [manualPath, setManualPath] = useState('')
  const [error, setError] = useState('')
  const [preparing, setPreparing] = useState(false)
  const [preparationMessage, setPreparationMessage] = useState('')
  const mounted = useRef(true)
  const { busy, run } = useAction()
  useEffect(() => {
    mounted.current = true
    const stop = native?.onProgress((message) => { if (mounted.current) setPreparationMessage(message) })
    return () => { mounted.current = false; stop?.() }
  }, [native])
  useEffect(() => {
    let current = true
    if (workspaceId !== undefined) host.call('runtime', { op: 'status', workspaceId }).then((value) => { if (current) setSnapshot(value) },
      (failure: unknown) => { if (current) setError(errorText(failure)) })
    if (native !== undefined) native.targets().then((value) => { if (current) setTargets(value) },
      (failure: unknown) => { if (current) setError(errorText(failure)) })
    return () => { current = false }
  }, [workspaceId, native])
  const accept = (value: RuntimeSnapshot): void => {
    if (mounted.current && value.workspaceId === workspaceId) { setSnapshot(value); setError('') }
  }
  const selected = snapshot?.selected[language]
  const candidates = snapshot?.candidates.filter(candidate => candidate.language === language) ?? []
  const prepare = (): void => {
    if (native === undefined) return
    run(async () => {
      setPreparationMessage(''); setPreparing(true)
      try {
        await native.prepare()
        const available = await native.targets()
        if (mounted.current) setTargets(available)
      } finally { if (mounted.current) setPreparing(false) }
    })
  }
  return <section className={css.section} data-settings-section="runtime">
    <h2 className={css.heading}>{t('settingsRuntime')}</h2>
    {targets !== undefined && native !== undefined && <Field label={t('settingsExecutionTarget')}>
      <Select label={t('settingsExecutionTarget')} value={targets.current.id} disabled={busy}
        items={targets.targets.map(target => ({ id: target.id, label: target.label }))}
        onChange={(targetId) => {
          if (targetId === targets.current.id) return
          run(async () => {
            const switched = await native.switchTarget({ targetId, ...workspaceId === undefined ? {} : { workspaceId } })
            if (!switched.ok) throw new Error(switched.error ?? t('settingsMissingCapability'))
          })
        }} /></Field>}
    <p className={css.muted}>{t('settingsEnvironmentNote')}</p>
    {error !== '' && <Notice tone="error">{error}</Notice>}
    {native !== undefined && <div className={css.actions}>
      <Button variant="outline" disabled={busy} onClick={prepare}>{t('settingsPrepareEnvironments')}</Button>
    </div>}
    {preparing && <Notice tone="status">{preparationMessage || t('settingsPreparingEnvironments')}</Notice>}
    {workspaceId === undefined ? <Notice>{t('settingsOpenProject')}</Notice> : snapshot === undefined ? error === '' && <Loading label={t('loading')} /> : <>
      <div className={css.grid}>
        <Field label={t('settingsLanguage')}><Select label={t('settingsLanguage')} value={language} disabled={busy}
          items={languages.map(id => ({ id, label: id }))} onChange={(value) => { setLanguage(value) }} /></Field>
        <Field label={t('settingsRuntime')}><Select label={t('settingsRuntime')} value={selected?.path ?? ''} disabled={busy}
          items={[{ id: '', label: t('settingsAutomatic') }, ...candidates.map(candidate => ({ id: candidate.path,
            label: `${candidate.path}${candidate.version === null ? '' : ` · ${candidate.version}`}`, disabled: !candidate.ready }))]}
          onChange={(path) => { run(async () => { accept(await host.call('runtime', { op: 'select', workspaceId, language, path: path || null })) }, t('settingsApplied')) }} /></Field>
      </div>
      <div className={css.actions}>
        <Button variant="outline" disabled={busy} onClick={() => { run(async () => { accept(await host.call('runtime', { op: 'discover', workspaceId })) }) }}>{t('settingsDiscoverEnvironments')}</Button>
      </div>
      <Field label={t('settingsEnvironmentPath')}><input className={css.input} aria-label={t('settingsEnvironmentPath')} value={manualPath}
        spellCheck={false} onChange={(event) => { setManualPath(event.target.value) }} /></Field>
      <div className={css.actions}><Button variant="outline" disabled={busy || manualPath.trim() === ''} onClick={() => { run(async () => {
        accept(await host.call('runtime', { op: 'probe', workspaceId, language, path: manualPath.trim() }))
      }) }}>{t('settingsProbeEnvironment')}</Button></div>
      {candidates.map(candidate => <article className={css.card} key={candidate.id} data-candidate={candidate.path}>
        <div className={css.row}>
          <span className={css.path}>{candidate.path}</span>
          <span className={candidate.ready ? css.muted : css.errorText}>{t(candidate.ready ? 'settingsReadyCapability' : 'settingsMissingCapability')}</span>
        </div>
        <p className={css.muted}>{[candidate.version ?? candidate.platform, candidate.source].join(' · ')}</p>
        {candidate.error !== undefined && <p className={clsx(css.body, css.errorText)}>{candidate.error}</p>}
        {candidate.capabilities.length > 0 && <ul className={css.list}>{candidate.capabilities.map(capability => <li key={capability.name}>
          {capability.name} · {t(capability.ready ? 'settingsReadyCapability' : 'settingsMissingCapability')}{capability.detail === undefined ? '' : ` · ${capability.detail}`}
        </li>)}</ul>}
      </article>)}
    </>}
    <OptionalModules />
  </section>
}
