/** Security section: the mode new chats start in, and the commands and MCP tools the current project allows without asking. */
import { useEffect, useRef, useState } from 'react'
import type { PermissionMode, SecurityStatus } from '../../shared/rpc.ts'
import { setPrefs, usePrefs } from '../prefs.ts'
import { host } from '../rpc.ts'
import { Button } from '../ui/Button.tsx'
import { Switch } from '../ui/Switch.tsx'
import { toast } from '../ui/toasts.tsx'
import { useT } from './messages.ts'
import { Loading, Notice, Select, SettingRow, errorText, useAction } from './parts.tsx'
import type { SettingsWorkspace } from './parts.tsx'
import css from './sections.module.css'

/**
 * @param props.workspace Project whose remembered approvals are shown.
 * @returns The section.
 */
export function SecuritySection({ workspace }: { workspace: SettingsWorkspace }): JSX.Element {
  const t = useT()
  const prefs = usePrefs()
  const workspaceId = workspace?.workspaceId
  const [status, setStatus] = useState<SecurityStatus | undefined>()
  const [error, setError] = useState('')
  const current = useRef(workspaceId)
  current.current = workspaceId
  const { busy, run } = useAction()
  useEffect(() => {
    setStatus(undefined); setError('')
    host.call('security.status', { workspaceId: workspaceId ?? null }).then(
      (value) => { if (current.current === workspaceId) setStatus(value) },
      (failure: unknown) => { if (current.current === workspaceId) setError(errorText(failure)) },
    )
  }, [workspaceId])

  const modes: { id: PermissionMode; label: string }[] = [
    { id: 'read-only', label: t('securityModeReadOnly') }, { id: 'ask', label: t('securityModeAsk') }, { id: 'auto', label: t('securityModeAuto') },
  ]
  const rules = status === undefined ? [] : [
    ...status.rules.commands.map(rule => ({ kind: 'command' as const, rule })),
    ...status.rules.tools.map(rule => ({ kind: 'tool' as const, rule })),
  ]
  return <section className={css.section} data-settings-section="security">
    <h2 className={css.heading}>{t('settingsSecurity')}</h2>
    <p className={css.muted}>{t('settingsSecurityNote')}</p>
    {error !== '' && <Notice tone="error">{error}</Notice>}
    {status === undefined ? error === '' && <Loading label={t('loading')} /> : <>
      <div className={css.group}>
        <SettingRow title={t('securityDefaultMode')} description={t('securityDefaultModeNote')}>
          <Select label={t('securityDefaultMode')} value={status.defaultMode} items={modes} disabled={busy} onChange={(mode) => {
            run(async () => {
              const next = await host.call('security.setDefault', { mode })
              setStatus(previous => previous === undefined ? previous : { ...previous, defaultMode: next.defaultMode })
            }, t('settingsApplied'))
          }} />
        </SettingRow>
        <SettingRow title={t('securityAutoKeep')} description={t('securityAutoKeepNote')}>
          <Switch label={t('securityAutoKeep')} checked={prefs.autoKeepChanges} onChange={(autoKeepChanges) => {
            setPrefs({ autoKeepChanges }).catch((failure: unknown) => { toast(errorText(failure), { tone: 'error' }) })
          }} />
        </SettingRow>
      </div>
      <h3 className={css.subheading}>{t('securityRules')}</h3>
      <p className={css.muted}>{t('securityRulesNote')}</p>
      {workspaceId === undefined ? <Notice>{t('settingsOpenProject')}</Notice>
        : rules.length === 0 ? <p className={css.muted}>{t('securityNoRules')}</p>
          : <ul className={css.items}>
            {rules.map(({ kind, rule }) => (
              <li key={`${kind}:${rule}`} className={css.item}>
                <span className={css.itemText}>
                  <span className={css.itemTitle}><code>{kind === 'tool' ? rule.replace(/^mcp__/u, '').replace('__', ': ') : rule}</code></span>
                  <span className={css.muted}>{t(kind === 'command' ? 'securityRuleCommand' : 'securityRuleTool')}</span>
                </span>
                <Button variant="ghost" size="sm" disabled={busy} onClick={() => {
                  run(async () => { setStatus(await host.call('security.removeRule', { workspaceId, kind, rule })) })
                }}>{t('securityRemoveRule')}</Button>
              </li>
            ))}
          </ul>}
    </>}
  </section>
}
