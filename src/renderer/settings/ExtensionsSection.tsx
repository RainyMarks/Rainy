/** Skills & MCP section: discovered skills and the MCP servers every chat may use. */
import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react'
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
import { emptyServerForm, isWindowsMcp, parseServerForm, serverFormOf, windowsMcpForm } from './mcp-form.ts'
import type { McpServerForm } from './mcp-form.ts'
import { MCP_JSON_MAX_LENGTH, decodeConfigFile, importKey, looksLikeMcpJson, maskedCommand, maskedUrl, parseMcpJson } from './mcp-json.ts'
import { Field, Loading, Notice, errorText, useAction } from './parts.tsx'
import type { SettingsWorkspace } from './parts.tsx'
import css from './sections.module.css'

const stateCopy: Record<McpServerStatus['state'], { key: SettingsMessage; tone: TagTone }> = {
  disabled: { key: 'mcpStateDisabled', tone: 'neutral' },
  connecting: { key: 'mcpStateConnecting', tone: 'warning' },
  ready: { key: 'mcpStateReady', tone: 'success' },
  error: { key: 'mcpStateError', tone: 'danger' },
}

const IMPORT_EXAMPLE = `{
  "mcpServers": {
    "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"] }
  }
}`

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
  const [form, setForm] = useState<{ value: McpServerForm; previousName?: string | undefined; desktop?: boolean } | undefined>()
  const [formError, setFormError] = useState<string | undefined>()
  const [removing, setRemoving] = useState<string | undefined>()
  /** Pasted MCP JSON and the servers read from it that the user unchecked, by `importKey`. */
  const [importing, setImporting] = useState<{ text: string; skipped: ReadonlySet<string> } | undefined>()
  const [importError, setImportError] = useState<string | undefined>()
  const fileInput = useRef<HTMLInputElement>(null)
  const { busy, run } = useAction()
  const mounted = useRef(true)
  const transportId = useId()
  // Windows-MCP drives the PC's own desktop, which a Host on an SSH server cannot reach.
  const environment = window.__RAINY_AGENT__?.environment
  const desktopHost = environment === 'Windows' || environment === 'WSL' ? environment : undefined

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
  /** Counts file reads, so one that finishes after the panel closed or another read started is dropped. */
  const fileRead = useRef(0)
  const open = (value: McpServerForm, previousName?: string, desktop = false): void => {
    setForm({ value, previousName, desktop }); setFormError(undefined); setRemoving(undefined); setImporting(undefined); setImportError(undefined)
    fileRead.current++
  }
  const startImport = (text: string): void => {
    setImporting({ text, skipped: new Set() }); setImportError(undefined); setForm(undefined); setFormError(undefined); setRemoving(undefined)
    fileRead.current++
  }
  const closeImport = (): void => { setImporting(undefined); setImportError(undefined); fileRead.current++ }
  // The configured servers change whenever one is saved, also while an import runs; as text they compare by value.
  const configured = JSON.stringify(servers)
  const importText = importing?.text
  const preview = useMemo(() => importText === undefined || importText.trim() === '' ? undefined
    : parseMcpJson(importText, JSON.parse(configured) as McpServerConfig[], environment), [importText, configured, environment])
  const chosen = preview?.ok === true ? preview.servers.filter(entry => importing?.skipped.has(importKey(entry)) !== true) : []
  const importServers = (): void => {
    if (chosen.length === 0) return
    run(async () => {
      let done = 0
      for (const entry of chosen) {
        try {
          merge(await host.call('extensions.saveServer', { server: entry.server }))
        } catch (failure) {
          // The servers already saved now match configured ones, so trying again leaves them out.
          throw new Error(t('mcpImportFailed', { name: entry.server.name, count: done, error: errorText(failure) }))
        }
        done++
      }
      if (mounted.current) closeImport()
    }, t('mcpImportDone', { count: chosen.length }))
  }
  const readFile = (file: File | undefined): void => {
    if (file === undefined) return
    // A file far larger than any configuration is refused before it is read; the text limit is in characters.
    if (file.size > MCP_JSON_MAX_LENGTH * 4) { setImportError(t('mcpJsonTooLarge')); return }
    const read = ++fileRead.current
    file.arrayBuffer().then((buffer) => {
      if (mounted.current && fileRead.current === read) { setImporting({ text: decodeConfigFile(new Uint8Array(buffer)), skipped: new Set() }); setImportError(undefined) }
    }).catch((failure: unknown) => { if (mounted.current && fileRead.current === read) setImportError(t('mcpImportFileError', { error: errorText(failure) })) })
  }
  const edit = (change: Partial<McpServerForm>): void => {
    setForm(current => current === undefined ? current : { ...current, value: { ...current.value, ...change } })
  }
  const save = (): void => {
    if (form === undefined) return
    // The list's switch may have changed the server while the form was open; the form does not edit that state.
    const enabled = servers.find(server => server.name === form.previousName)?.enabled ?? form.value.enabled
    const parsed = parseServerForm({ ...form.value, enabled }, servers.filter(server => server.name !== form.previousName).map(server => server.name))
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
          {desktopHost !== undefined && status !== undefined && !servers.some(isWindowsMcp) && <Button size="sm" variant="outline" disabled={busy}
            data-add-desktop onClick={() => { open(windowsMcpForm(desktopHost, servers.map(server => server.name)), undefined, true) }}>{t('mcpAddDesktop')}</Button>}
          <Button size="sm" variant="outline" disabled={busy || status === undefined} data-import-json onClick={() => { startImport('') }}>{t('mcpImport')}</Button>
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
              <Button size="sm" disabled={busy} onClick={() => { open(serverFormOf(server), server.name, isWindowsMcp(server)) }}>{t('mcpEdit')}</Button>
              <Button size="sm" disabled={busy} onClick={() => { remove(server) }}>{t(removing === server.name ? 'mcpRemoveConfirm' : 'mcpRemove')}</Button>
            </div>
          </li>
        })}</ul>)}
      {importing !== undefined && <div className={css.subcard} data-mcp-import>
        <h4 className={css.subheading}>{t('mcpImportTitle')}</h4>
        <p className={css.muted}>{t('mcpImportHint')}</p>
        <textarea className={css.textarea} aria-label={t('mcpImportLabel')} value={importing.text} rows={8} spellCheck={false} disabled={busy}
          placeholder={IMPORT_EXAMPLE} onChange={(event) => { setImporting({ text: event.target.value, skipped: new Set() }); setImportError(undefined) }} />
        <div className={css.actions}>
          <Button size="sm" variant="outline" disabled={busy} onClick={() => { fileInput.current?.click() }}>{t('mcpImportFile')}</Button>
          <input ref={fileInput} type="file" hidden accept=".json,.jsonc,.txt,application/json" aria-label={t('mcpImportFile')}
            onChange={(event) => { const file = event.target.files?.[0]; event.target.value = ''; readFile(file) }} />
        </div>
        {importError !== undefined && <Notice tone="error">{importError}</Notice>}
        {preview !== undefined && !preview.ok && <Notice tone="error">{t(preview.error.key, preview.error.vars)}</Notice>}
        {preview?.ok === true && <>
          {preview.notices.map(notice => <p key={notice.key} className={css.warnText}>{t(notice.key, notice.vars)}</p>)}
          {preview.servers.length > 0 && <p className={css.muted}>{t('mcpImportFound', { count: preview.servers.length })}</p>}
          <ul className={css.items}>{preview.servers.map((entry) => {
            const { server, notes } = entry
            const key = importKey(entry)
            return <li key={key} className={css.item} data-import-server={server.name}>
            <div className={css.itemText}>
              <div className={css.itemTitle}>
                <Checkbox label={server.name} checked={!importing.skipped.has(key)} title={t('mcpImportInclude', { name: server.name })} disabled={busy}
                  onChange={(checked) => {
                    setImporting(current => {
                      if (current === undefined) return current
                      const skipped = new Set(current.skipped)
                      if (checked) skipped.delete(key); else skipped.add(key)
                      return { ...current, skipped }
                    })
                  }} />
                <Tag tone="outline">{server.transport === 'stdio' ? 'stdio' : 'HTTP'}</Tag>
                {!server.enabled && <Tag tone="neutral">{t('mcpStateDisabled')}</Tag>}
              </div>
              <div className={css.path}>{server.transport === 'stdio' ? maskedCommand([server.command ?? '', ...server.args ?? []]) : maskedUrl(server.url ?? '')}</div>
              {server.env !== undefined && <div className={css.muted}>{t('mcpImportEnv', { keys: Object.keys(server.env).join(', ') })}</div>}
              {server.headers !== undefined && <div className={css.muted}>{t('mcpImportHeaders', { keys: Object.keys(server.headers).join(', ') })}</div>}
              {notes.map((note, position) => <p key={position} className={css.warnText}>{t(note.key, note.vars)}</p>)}
            </div>
          </li>
          })}</ul>
          {preview.problems.length > 0 && <div data-import-problems>
            <p className={css.muted}>{t('mcpImportSkipped')}</p>
            <ul className={css.list}>{preview.problems.map((problem, position) => <li key={position} className={css.errorText}>{t(problem.key, problem.vars)}</li>)}</ul>
          </div>}
        </>}
        <div className={css.actions}>
          <Button variant="primary" disabled={busy || chosen.length === 0} onClick={importServers}>{t('mcpImportSubmit', { count: chosen.length })}</Button>
          <Button disabled={busy} onClick={closeImport}>{t('settingsCancel')}</Button>
        </div>
      </div>}
      {form !== undefined && <div className={css.subcard} data-server-form onPasteCapture={(event) => {
        // MCP JSON pasted into an empty new server form is imported instead of landing in one field; nothing typed is lost.
        const typed = [form.value.name, form.value.command, form.value.args, form.value.env, form.value.url, form.value.headers].some(field => field.trim() !== '')
        if (form.previousName !== undefined || typed) return
        const text = event.clipboardData.getData('text')
        if (looksLikeMcpJson(text)) { event.preventDefault(); startImport(text) }
      }}>
        <h4 className={css.subheading}>{form.previousName === undefined ? t('mcpFormAdd') : t('mcpFormEdit', { name: form.previousName })}</h4>
        {form.desktop === true && <div data-desktop-note>
          <p className={css.muted}>{t('mcpDesktopNote')}</p>
          <Notice>{t('mcpDesktopSetup')}</Notice>
        </div>}
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
