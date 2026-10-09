/** Skills & MCP section: discovered skills and the MCP servers every chat may use. */
import { useCallback, useEffect, useId, useRef, useState } from 'react'
import type { ExtensionsStatus, McpServerConfig, McpServerStatus } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { Button } from '../ui/Button.tsx'
import { Checkbox } from '../ui/Checkbox.tsx'
import { SegmentedControl } from '../ui/SegmentedControl.tsx'
import { Switch } from '../ui/Switch.tsx'
import { Tag } from '../ui/Tag.tsx'
import type { TagTone } from '../ui/Tag.tsx'
import { useT } from './messages.ts'
import type { SettingsMessage } from './messages.ts'
import { emptyServerForm, parseServerForm, serverFormOf } from './mcp-form.ts'
import type { McpServerForm } from './mcp-form.ts'
import { Field, Loading, Notice, errorText, useAction } from './parts.tsx'
import type { SettingsWorkspace } from './parts.tsx'
import css from './sections.module.css'

const stateCopy: Record<McpServerStatus['state'], { key: SettingsMessage; tone: TagTone }> = {
  disabled: { key: 'mcpStateDisabled', tone: 'neutral' },
  connecting: { key: 'mcpStateConnecting', tone: 'warning' },
  ready: { key: 'mcpStateReady', tone: 'success' },
  error: { key: 'mcpStateError', tone: 'danger' },
}

function skillPath(root: string, name: string): string {
  const separator = root.includes('\\') && !root.includes('/') ? '\\' : '/'
  return [root.replace(/[\\/]+$/, ''), '.rainy', 'skills', name, 'SKILL.md'].join(separator)
}

/**
 * Discovered skills (read-only), configured MCP servers with live status, and the server form.
 * Saved changes apply to the next model request of every chat.
 * @param props.workspace Project whose `.rainy/skills` folder is listed.
 * @returns The section.
 */
export function ExtensionsSection({ workspace }: { workspace: SettingsWorkspace }): JSX.Element {
  const t = useT()
  const cwd = workspace?.path
  const [status, setStatus] = useState<ExtensionsStatus | undefined>()
  const [error, setError] = useState('')
  const [form, setForm] = useState<{ value: McpServerForm; previousName?: string | undefined } | undefined>()
  const [formError, setFormError] = useState<string | undefined>()
  const [removing, setRemoving] = useState<string | undefined>()
  const { busy, run } = useAction()
  const mounted = useRef(true)
  const transportId = useId()

  // Server changes come back with the Host's skill list for whichever folder it last read, so keep the one read for this project.
  const merge = useCallback((value: ExtensionsStatus): void => {
    if (mounted.current) setStatus(previous => ({ ...value, skills: previous?.skills ?? value.skills }))
  }, [])
  const read = useCallback(async (): Promise<void> => {
    const value = await host.call('extensions.status', cwd === undefined ? {} : { cwd })
    if (mounted.current) { setStatus(value); setError('') }
  }, [cwd])
  useEffect(() => {
    mounted.current = true
    read().catch((failure: unknown) => { if (mounted.current) setError(errorText(failure)) })
    const stop = host.on('extensions.changed', merge)
    return () => { mounted.current = false; stop() }
  }, [read, merge])

  const servers = status?.servers ?? []
  const live = (name: string): McpServerStatus | undefined => status?.status.find(entry => entry.name === name)
  const open = (value: McpServerForm, previousName?: string): void => {
    setForm({ value, previousName }); setFormError(undefined); setRemoving(undefined)
  }
  const edit = (change: Partial<McpServerForm>): void => {
    setForm(current => current === undefined ? current : { ...current, value: { ...current.value, ...change } })
  }
  const save = (): void => {
    if (form === undefined) return
    const parsed = parseServerForm(form.value, servers.filter(server => server.name !== form.previousName).map(server => server.name))
    if (!parsed.ok) { setFormError(t(parsed.error, parsed.vars)); return }
    setFormError(undefined)
    run(async () => {
      merge(await host.call('extensions.saveServer', { server: parsed.server, ...form.previousName === undefined ? {} : { previousName: form.previousName } }))
      if (mounted.current) setForm(undefined)
    }, t('settingsSaved'))
  }
  const toggle = (server: McpServerConfig, enabled: boolean): void => {
    run(async () => { merge(await host.call('extensions.saveServer', { server: { ...server, enabled }, previousName: server.name })) })
  }
  const remove = (server: McpServerConfig): void => {
    if (removing !== server.name) { setRemoving(server.name); return }
    setRemoving(undefined)
    run(async () => {
      merge(await host.call('extensions.removeServer', { name: server.name }))
      if (mounted.current && form?.previousName === server.name) setForm(undefined)
    }, t('mcpRemoved', { name: server.name }))
  }

  const projectRoot = workspace?.path ?? t('skillsProject')
  const editedTools = form === undefined ? [] : live(form.previousName ?? '')?.tools ?? []
  const toolNames = form === undefined ? [] : [...new Set([...editedTools.map(tool => tool.name), ...form.value.tools])]

  return <section className={css.section} data-settings-section="extensions">
    <h2 className={css.heading}>{t('extensionsTitle')}</h2>
    <p className={css.muted}>{t('extensionsNote')}</p>
    {error !== '' && <Notice tone="error">{error}</Notice>}
    <article className={css.card} data-skills>
      <div className={css.row}>
        <h3 className={css.subheading}>{t('skillsTitle')}</h3>
        <Button size="sm" variant="outline" disabled={busy} onClick={() => { run(read) }}>{t('settingsRefresh')}</Button>
      </div>
      <p className={css.muted}>{t('skillsHint', {
        project: skillPath(projectRoot, t('skillsName')), user: `~/.rainy-agent/skills/${t('skillsName')}/SKILL.md`,
      })}</p>
      {status === undefined ? error === '' && <Loading label={t('loading')} />
        : status.skills.length === 0 ? <p className={css.muted}>{t('settingsNoSkills')}</p>
          : <ul className={css.items}>{status.skills.map(skill => <li key={skill.id} className={css.item} data-skill={skill.id}>
            <div className={css.itemText}>
              <div className={css.itemTitle}>
                <span>{skill.name}</span>
                <Tag tone="info">{t(skill.scope === 'project' ? 'skillScopeProject' : 'skillScopeUser')}</Tag>
              </div>
              {skill.description !== '' && <div className={css.muted}>{skill.description}</div>}
              <div className={css.path}>{skill.path}</div>
            </div>
          </li>)}</ul>}
    </article>
    <article className={css.card} data-mcp>
      <div className={css.row}>
        <h3 className={css.subheading}>{t('mcpTitle')}</h3>
        <div className={css.actions}>
          {status?.idaAvailable === true && !servers.some(server => server.name === 'ida') && <Button size="sm" variant="outline" disabled={busy}
            onClick={() => { run(async () => { merge(await host.call('extensions.addIda')) }, t('mcpIdaAdded')) }}>{t('mcpAddIda')}</Button>}
          <Button size="sm" variant="outline" disabled={busy || status === undefined} onClick={() => { open(emptyServerForm()) }}>{t('mcpAdd')}</Button>
        </div>
      </div>
      {status !== undefined && (servers.length === 0 ? <p className={css.muted}>{t('mcpEmpty')}</p>
        : <ul className={css.items}>{servers.map((server) => {
          const entry = live(server.name)
          const state = server.enabled ? entry?.state ?? 'connecting' : 'disabled'
          return <li key={server.name} className={css.item} data-server={server.name}>
            <div className={css.itemText}>
              <div className={css.itemTitle}>
                <span>{server.name}</span>
                <Tag tone="outline">{server.transport === 'stdio' ? 'stdio' : 'HTTP'}</Tag>
                <Tag tone={stateCopy[state].tone}>{t(stateCopy[state].key)}</Tag>
              </div>
              <div className={css.path}>{server.transport === 'stdio' ? [server.command ?? '', ...server.args ?? []].join(' ') : server.url}</div>
              {entry !== undefined && entry.tools.length > 0 && <div className={css.muted}>{t('mcpTools', {
                enabled: entry.tools.filter(tool => tool.enabled).length, total: entry.tools.length,
              })}</div>}
              {state === 'error' && entry?.error !== undefined && <p className={css.errorText}>{entry.error}</p>}
            </div>
            <div className={css.actions}>
              <Switch checked={server.enabled} label={t('mcpEnable', { name: server.name })} disabled={busy}
                onChange={(enabled) => { toggle(server, enabled) }} />
              <Button size="sm" disabled={busy} onClick={() => { open(serverFormOf(server), server.name) }}>{t('mcpEdit')}</Button>
              <Button size="sm" disabled={busy} onClick={() => { remove(server) }}>{t(removing === server.name ? 'mcpRemoveConfirm' : 'mcpRemove')}</Button>
            </div>
          </li>
        })}</ul>)}
      {form !== undefined && <div className={css.subcard} data-server-form>
        <h4 className={css.subheading}>{form.previousName === undefined ? t('mcpFormAdd') : t('mcpFormEdit', { name: form.previousName })}</h4>
        <div className={css.grid}>
          <Field label={t('mcpName')}><input className={css.input} aria-label={t('mcpName')} value={form.value.name} spellCheck={false}
            autoComplete="off" maxLength={32} onChange={(event) => { edit({ name: event.target.value }) }} /></Field>
          <div className={css.field}><span>{t('mcpTransport')}</span>
            <SegmentedControl id={transportId} label={t('mcpTransport')} value={form.value.transport}
              options={[{ value: 'stdio', label: t('mcpStdio') }, { value: 'streamable-http', label: t('mcpHttp') }]}
              onChange={(transport) => { edit({ transport }) }} /></div>
          {form.value.transport === 'stdio' ? <>
            <Field label={t('mcpCommand')} wide><input className={css.input} aria-label={t('mcpCommand')} value={form.value.command}
              spellCheck={false} autoComplete="off" onChange={(event) => { edit({ command: event.target.value }) }} /></Field>
            <Field label={t('mcpArgs')} wide><textarea className={css.textarea} aria-label={t('mcpArgs')} value={form.value.args} rows={3}
              spellCheck={false} onChange={(event) => { edit({ args: event.target.value }) }} /></Field>
            <Field label={t('mcpEnv')} wide><textarea className={css.textarea} aria-label={t('mcpEnv')} value={form.value.env} rows={3}
              spellCheck={false} onChange={(event) => { edit({ env: event.target.value }) }} /></Field>
          </> : <>
            <Field label={t('mcpUrl')} wide><input className={css.input} aria-label={t('mcpUrl')} value={form.value.url} type="url"
              placeholder="https://" spellCheck={false} autoComplete="off" onChange={(event) => { edit({ url: event.target.value }) }} /></Field>
            <Field label={t('mcpHeaders')} wide><textarea className={css.textarea} aria-label={t('mcpHeaders')} value={form.value.headers} rows={3}
              placeholder="Authorization: Bearer …" spellCheck={false} onChange={(event) => { edit({ headers: event.target.value }) }} /></Field>
          </>}
        </div>
        <fieldset className={css.tools}>
          <legend>{t('mcpToolsTitle')}</legend>
          {toolNames.length === 0 ? <p className={css.muted}>{t('mcpToolsUnknown')}</p> : <>
            <p className={css.muted}>{t('mcpToolsHint')}</p>
            <div className={css.toolGrid}>{toolNames.map(name => <Checkbox key={name} label={name} checked={form.value.tools.includes(name)}
              title={editedTools.find(tool => tool.name === name)?.description}
              onChange={(checked) => { edit({ tools: checked ? [...form.value.tools, name] : form.value.tools.filter(tool => tool !== name) }) }} />)}</div>
          </>}
        </fieldset>
        {formError !== undefined && <Notice tone="error">{formError}</Notice>}
        <div className={css.actions}>
          <Button variant="primary" disabled={busy} onClick={save}>{t('settingsSave')}</Button>
          <Button disabled={busy} onClick={() => { setForm(undefined); setFormError(undefined) }}>{t('settingsCancel')}</Button>
        </div>
      </div>}
    </article>
  </section>
}
