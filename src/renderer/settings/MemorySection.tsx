/** Memory section: the current project's notes, their switches, editing and removal. */
import { useCallback, useEffect, useRef, useState } from 'react'
import type { ProjectMemoryStatus } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { Button } from '../ui/Button.tsx'
import { Switch } from '../ui/Switch.tsx'
import { Tag } from '../ui/Tag.tsx'
import { useLocale } from '../i18n.ts'
import { useT } from './messages.ts'
import { Field, Loading, Notice, SettingRow, errorText, useAction } from './parts.tsx'
import type { SettingsWorkspace } from './parts.tsx'
import css from './sections.module.css'

/**
 * Inspect, edit or remove the current project's memory. Edits carry the revision they started from.
 * @param props.workspace Project whose memory is shown.
 * @returns The section.
 */
export function MemorySection({ workspace }: { workspace: SettingsWorkspace }): JSX.Element {
  const t = useT()
  const locale = useLocale()
  const workspaceId = workspace?.workspaceId
  const [snapshot, setSnapshot] = useState<ProjectMemoryStatus | undefined>()
  const [error, setError] = useState('')
  const [edit, setEdit] = useState<{ id: string; text: string; revision: number } | undefined>()
  const [clearRequested, setClearRequested] = useState(false)
  const current = useRef(workspaceId)
  current.current = workspaceId
  const { busy, run } = useAction()
  const show = useCallback((value: ProjectMemoryStatus, project: typeof workspaceId): void => {
    if (current.current === project) { setSnapshot(value); setError('') }
  }, [])
  const reload = useCallback(async (): Promise<void> => {
    if (workspaceId === undefined) return
    show(await host.call('memory.status', { workspaceId }), workspaceId)
  }, [workspaceId, show])
  useEffect(() => {
    setSnapshot(undefined); setError(''); setEdit(undefined); setClearRequested(false)
    if (workspaceId === undefined) return undefined
    reload().catch((failure: unknown) => { if (current.current === workspaceId) setError(errorText(failure)) })
    const stop = host.on('memory.changed', (event) => {
      if (event.workspaceId === workspaceId) reload().catch(() => undefined)
    })
    return () => { stop() }
  }, [workspaceId, reload])
  const apply = (operation: () => Promise<ProjectMemoryStatus>, success = t('settingsApplied')): void => {
    run(async () => { show(await operation(), workspaceId) }, success)
  }
  return <section className={css.section} data-settings-section="memory">
    <h2 className={css.heading}>{t('settingsMemory')}</h2>
    <p className={css.muted}>{t('settingsMemoryNote')}</p>
    {workspaceId === undefined ? <Notice>{t('settingsOpenProject')}</Notice> : <>
      <div className={css.actions}><Button variant="outline" disabled={busy} onClick={() => { run(reload) }}>{t('settingsRefresh')}</Button></div>
      {error !== '' && <Notice tone="error">{error}</Notice>}
      {snapshot === undefined ? error === '' && <Loading label={t('loading')} /> : <>
        <div className={css.rows}>
          <SettingRow title={t('settingsMemoryUse')}>
            <Switch label={t('settingsMemoryUse')} checked={snapshot.enabled} disabled={busy} onChange={(enabled) => {
              apply(() => host.call('memory.setEnabled', { workspaceId, enabled }))
            }} />
          </SettingRow>
          <SettingRow title={t('settingsMemoryGenerate')}>
            <Switch label={t('settingsMemoryGenerate')} checked={snapshot.generationEnabled} disabled={busy} onChange={(generationEnabled) => {
              apply(() => host.call('memory.setEnabled', { workspaceId, generationEnabled }))
            }} />
          </SettingRow>
        </div>
        {snapshot.generating === true ? <Notice tone="status">{t('settingsMemoryGenerating')}</Notice>
          : snapshot.pending === true ? <Notice tone="status">{t('settingsMemoryPending')}</Notice> : null}
        {snapshot.error !== undefined && <Notice tone="error">{snapshot.error}</Notice>}
        {typeof snapshot.updatedAt === 'string' && <p className={css.muted}>{t('settingsMemoryUpdated', {
          time: new Date(snapshot.updatedAt).toLocaleString(locale === 'zh' ? 'zh-CN' : 'en-US'),
        })}</p>}
        {snapshot.items.length === 0 && <Notice>{t('settingsMemoryEmpty')}</Notice>}
        {snapshot.items.map(item => <article className={css.card} key={item.id} data-memory={item.id}>
          {edit?.id === item.id
            ? <Field label={t('settingsMemoryText')}><textarea className={css.textarea} aria-label={t('settingsMemoryText')} value={edit.text}
              onChange={(event) => { setEdit({ ...edit, text: event.target.value }) }} /></Field>
            : <p className={css.body}>{item.text}</p>}
          {item.editedByUser === true && <div><Tag tone="neutral">{t('settingsMemoryEdited')}</Tag></div>}
          {item.sources.map((source, index) => <p className={css.muted} key={`${source.sessionId}:${source.seq}:${index}`}>
            {t('settingsMemorySource', { session: source.sessionId, seq: source.seq, target: source.executionTargetId })}
            {source.file === undefined ? '' : ` · ${source.file.path}`}
          </p>)}
          <div className={css.actions}>
            {edit?.id === item.id ? <>
              <Button variant="primary" disabled={busy || edit.text.trim() === ''} onClick={() => {
                const { id, text, revision } = edit
                run(async () => {
                  show(await host.call('memory.edit', { workspaceId, id, text, expectedRevision: revision }), workspaceId)
                  setEdit(undefined)
                }, t('settingsSaved'))
              }}>{t('settingsSave')}</Button>
              <Button disabled={busy} onClick={() => { setEdit(undefined) }}>{t('settingsCancel')}</Button>
            </> : <>
              <Button variant="outline" disabled={busy} onClick={() => { setEdit({ id: item.id, text: item.text, revision: snapshot.revision }) }}>{t('settingsMemoryEdit')}</Button>
              <Button disabled={busy} onClick={() => { apply(() => host.call('memory.delete', { workspaceId, id: item.id })) }}>{t('settingsMemoryDelete')}</Button>
            </>}
          </div>
        </article>)}
        {snapshot.items.length > 0 && <div className={css.actions}>
          {clearRequested ? <>
            <Button variant="outline" disabled={busy} onClick={() => {
              setClearRequested(false); setEdit(undefined)
              apply(() => host.call('memory.clear', { workspaceId }))
            }}>{t('settingsMemoryClearConfirm')}</Button>
            <Button disabled={busy} onClick={() => { setClearRequested(false) }}>{t('settingsCancel')}</Button>
          </> : <Button disabled={busy} onClick={() => { setClearRequested(true) }}>{t('settingsMemoryClear')}</Button>}
        </div>}
      </>}
    </>}
  </section>
}
