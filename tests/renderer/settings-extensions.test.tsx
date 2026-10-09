// @vitest-environment happy-dom
/** Skills & MCP lists discovered skills and configured servers, and validates the server form before the Host sees it. */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ExtensionsStatus, McpServerConfig } from '../../src/shared/rpc.ts'
import { ExtensionsSection } from '../../src/renderer/settings/ExtensionsSection.tsx'
import { emptyServerForm, parseServerForm, serverFormOf } from '../../src/renderer/settings/mcp-form.ts'
import { settingsMessages } from '../../src/renderer/settings/messages.ts'
import {
  all, button, cleanup, click, control, emit, fakeHost, findButton, handle, hasText, listenerCount, render, toast, type, waitFor,
} from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const zh = settingsMessages.zh
const workspace = { workspaceId: 'a' as WorkspaceId, path: '/project-a', title: 'A' }
const files: McpServerConfig = { name: 'files', enabled: true, transport: 'stdio', command: 'npx', args: ['-y', '@mcp/files'],
  env: { ROOT: '/tmp' }, tools: ['read_file'] }

function extensions(change: Partial<ExtensionsStatus> = {}): ExtensionsStatus {
  return {
    skills: [
      { id: 'project/pwn', scope: 'project', name: 'pwn', description: 'Exploit development workflow', path: '/project-a/.rainy/skills/pwn/SKILL.md' },
      { id: 'user/notes', scope: 'user', name: 'notes', description: '', path: '/home/u/.rainy-agent/skills/notes/SKILL.md' },
    ],
    servers: [files],
    status: [{ name: 'files', state: 'ready', tools: [
      { name: 'read_file', description: 'Read a file', enabled: true }, { name: 'write_file', description: 'Write a file', enabled: false }] }],
    idaAvailable: false,
    ...change,
  }
}

let status: ExtensionsStatus
const calls = (method: string) => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)
const form = (): HTMLElement => document.querySelector<HTMLElement>('[data-server-form]')!
const row = (name: string): HTMLElement => document.querySelector<HTMLElement>(`[data-server="${name}"]`)!

beforeEach(async () => {
  status = extensions()
  handle('extensions.status', () => status)
  handle('extensions.saveServer', ({ server, previousName }) => {
    status = { ...status, skills: [], servers: [...status.servers.filter(entry => entry.name !== (previousName ?? server.name)), server] }
    return status
  })
  handle('extensions.removeServer', ({ name }) => { status = { ...status, skills: [], servers: status.servers.filter(entry => entry.name !== name) }; return status })
  await emit('prefs.changed', { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true })
})
afterEach(async () => { await cleanup() })

it('lists project and user skills for the current project with where to add more', async () => {
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(document.querySelector('[data-skill="project/pwn"]')).not.toBeNull() })
  expect(calls('extensions.status')).toEqual([{ cwd: '/project-a' }])
  expect(document.querySelector('[data-skill="project/pwn"]')?.textContent)
    .toBe(`pwn${zh.skillScopeProject}Exploit development workflow/project-a/.rainy/skills/pwn/SKILL.md`)
  expect(document.querySelector('[data-skill="user/notes"]')?.textContent).toBe(`notes${zh.skillScopeUser}/home/u/.rainy-agent/skills/notes/SKILL.md`)
  expect(hasText('将 SKILL.md 放在 /project-a/.rainy/skills/<名称>/SKILL.md（项目）或 ~/.rainy-agent/skills/<名称>/SKILL.md（用户），Agent 会在相关时读取完整说明')).toBe(true)
  expect(hasText(zh.extensionsNote)).toBe(true)
})

it('reads skills without a project and names the project folder generically', async () => {
  status = extensions({ skills: [] })
  await render(<ExtensionsSection workspace={{ workspaceId: 'w' as WorkspaceId, path: 'C:\\ctf\\task', title: 'task' }} />)
  await waitFor(() => { expect(hasText(zh.settingsNoSkills)).toBe(true) })
  expect(hasText('将 SKILL.md 放在 C:\\ctf\\task\\.rainy\\skills\\<名称>\\SKILL.md（项目）或 ~/.rainy-agent/skills/<名称>/SKILL.md（用户），Agent 会在相关时读取完整说明')).toBe(true)
  await cleanup()
  handle('extensions.status', () => status)
  await render(<ExtensionsSection workspace={null} />)
  await waitFor(() => { expect(hasText(zh.settingsNoSkills)).toBe(true) })
  expect(calls('extensions.status')).toEqual([{}])
  expect(document.body.textContent).toContain('<项目>/.rainy/skills/<名称>/SKILL.md')
})

it('shows each server with its live state, tool count and connection error', async () => {
  status = extensions({
    servers: [files, { name: 'web', enabled: true, transport: 'streamable-http', url: 'https://mcp.example/mcp', tools: [] },
      { name: 'off', enabled: false, transport: 'stdio', command: 'off', tools: [] }],
    status: [...extensions().status, { name: 'web', state: 'error', error: '401 Unauthorized', tools: [] }],
  })
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(row('files')).not.toBeNull() })
  const details = (name: string): string | null | undefined => row(name).firstElementChild?.textContent
  expect(details('files')).toBe(`filesstdio${zh.mcpStateReady}npx -y @mcp/files1 / 2 个工具`)
  expect(details('web')).toBe(`webHTTP${zh.mcpStateError}https://mcp.example/mcp401 Unauthorized`)
  expect(row('off').textContent).toContain(zh.mcpStateDisabled)
  expect(button('启用 off').getAttribute('aria-checked')).toBe('false')
})

it('enables, disables and removes servers after confirmation', async () => {
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(row('files')).not.toBeNull() })
  await click(button('启用 files'))
  expect(calls('extensions.saveServer')).toEqual([{ server: { ...files, enabled: false }, previousName: 'files' }])
  await waitFor(() => { expect(button('启用 files').getAttribute('aria-checked')).toBe('false') })
  expect(document.querySelector('[data-skill="project/pwn"]')).not.toBeNull()
  await click(button(zh.mcpRemove, row('files')))
  expect(calls('extensions.removeServer')).toEqual([])
  await click(button(zh.mcpRemoveConfirm, row('files')))
  await waitFor(() => { expect(hasText(zh.mcpEmpty)).toBe(true) })
  expect(calls('extensions.removeServer')).toEqual([{ name: 'files' }])
  expect(toast).toHaveBeenCalledWith('已删除 files', { tone: 'success' })
})

it('validates names, commands and URLs before saving a new server', async () => {
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(row('files')).not.toBeNull() })
  await click(button(zh.mcpAdd))
  const error = (): string | null | undefined => form().querySelector('[role="alert"]')?.textContent
  const save = (): Promise<void> => click(button(zh.settingsSave, form()))
  expect(hasText(zh.mcpToolsUnknown, form())).toBe(true)
  await save()
  expect(error()).toBe(zh.mcpNameInvalid)
  await type(control(zh.mcpName), 'bad name')
  await save()
  expect(error()).toBe(zh.mcpNameInvalid)
  await type(control(zh.mcpName), 'files')
  await save()
  expect(error()).toBe('已有名为 files 的服务器')
  await type(control(zh.mcpName), 'web')
  await save()
  expect(error()).toBe(zh.mcpCommandRequired)
  await click(button(zh.mcpHttp))
  expect(all(`[aria-label="${zh.mcpCommand}"]`)).toEqual([])
  await save()
  expect(error()).toBe(zh.mcpUrlRequired)
  await type(control(zh.mcpUrl), 'ftp://mcp.example')
  await save()
  expect(error()).toBe(zh.mcpUrlRequired)
  expect(calls('extensions.saveServer')).toEqual([])
  await type(control(zh.mcpUrl), 'https://mcp.example/mcp')
  await save()
  await waitFor(() => { expect(document.querySelector('[data-server-form]')).toBeNull() })
  expect(calls('extensions.saveServer')).toEqual([{ server: { name: 'web', enabled: true, transport: 'streamable-http', url: 'https://mcp.example/mcp', tools: [] } }])
  expect(row('web')).not.toBeNull()
  expect(toast).toHaveBeenCalledWith(zh.settingsSaved, { tone: 'success' })
})

it('edits a server with its arguments, environment and an allow-list from the live tools', async () => {
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(row('files')).not.toBeNull() })
  await click(button(zh.mcpEdit, row('files')))
  expect(hasText('编辑 files', form())).toBe(true)
  expect(control<HTMLInputElement>(zh.mcpCommand).value).toBe('npx')
  expect(control<HTMLTextAreaElement>(zh.mcpArgs).value).toBe('-y\n@mcp/files')
  expect(control<HTMLTextAreaElement>(zh.mcpEnv).value).toBe('ROOT=/tmp')
  const boxes = all<HTMLInputElement>('input[type="checkbox"]', form())
  expect(boxes.map(box => `${box.parentElement?.textContent}:${String(box.checked)}`)).toEqual(['read_file:true', 'write_file:false'])
  expect(hasText(zh.mcpToolsHint, form())).toBe(true)
  await click(boxes[1]!)
  await type(control(zh.mcpEnv), 'ROOT=/tmp\nnot an assignment')
  await click(button(zh.settingsSave, form()))
  expect(form().querySelector('[role="alert"]')?.textContent).toBe('环境变量第 2 行应为 KEY=VALUE')
  await type(control(zh.mcpEnv), 'ROOT=/srv\n\nTOKEN=a=b')
  await type(control(zh.mcpArgs), '-y\n\n@mcp/files\n--verbose')
  await type(control(zh.mcpName), 'files2')
  await click(button(zh.settingsSave, form()))
  await waitFor(() => { expect(calls('extensions.saveServer')).toHaveLength(1) })
  expect(calls('extensions.saveServer')[0]).toEqual({ previousName: 'files', server: { name: 'files2', enabled: true, transport: 'stdio', command: 'npx',
    args: ['-y', '@mcp/files', '--verbose'], env: { ROOT: '/srv', TOKEN: 'a=b' }, tools: ['read_file', 'write_file'] } })
  await waitFor(() => { expect(row('files2')).not.toBeNull() })
  expect(document.querySelector('[data-server="files"]')).toBeNull()
})

it('adds the IDA server when the desktop found its launcher', async () => {
  status = extensions({ idaAvailable: true })
  handle('extensions.addIda', () => {
    status = { ...status, servers: [...status.servers, { name: 'ida', enabled: true, transport: 'stdio', command: 'uvx', tools: [] }] }
    return status
  })
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(findButton(zh.mcpAddIda)).toBeDefined() })
  await click(button(zh.mcpAddIda))
  await waitFor(() => { expect(row('ida')).not.toBeNull() })
  expect(calls('extensions.addIda')).toHaveLength(1)
  expect(toast).toHaveBeenCalledWith(zh.mcpIdaAdded, { tone: 'success' })
  expect(findButton(zh.mcpAddIda)).toBeUndefined()
})

it('follows extensions.changed while keeping the skills read for this project, and stops listening on unmount', async () => {
  const view = await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(row('files')).not.toBeNull() })
  await emit('extensions.changed', extensions({ skills: [], status: [{ name: 'files', state: 'connecting', tools: [] }] }))
  expect(row('files').textContent).toContain(zh.mcpStateConnecting)
  expect(document.querySelector('[data-skill="project/pwn"]')).not.toBeNull()
  await view.unmount()
  expect(listenerCount('extensions.changed')).toBe(0)
})

describe('parseServerForm', () => {
  const named = (name: string) => parseServerForm({ ...emptyServerForm(), name, command: 'run' }, [])
  it('accepts names of 1 to 32 letters, digits, underscores and hyphens', () => {
    for (const name of ['a', 'ida', 'my_server-2', 'x'.repeat(32), '  padded  ']) expect(named(name).ok).toBe(true)
    for (const name of ['', 'x'.repeat(33), 'a.b', 'a b', 'mcp/server', '服务器']) expect(named(name)).toEqual({ ok: false, error: 'mcpNameInvalid' })
  })
  it('requires a command for stdio and an http(s) URL for streamable HTTP', () => {
    expect(parseServerForm({ ...emptyServerForm(), name: 'a', command: '  ' }, [])).toEqual({ ok: false, error: 'mcpCommandRequired' })
    const http = { ...emptyServerForm(), name: 'a', transport: 'streamable-http' as const, command: 'ignored' }
    for (const url of ['', 'mcp.example', 'file:///tmp/mcp']) expect(parseServerForm({ ...http, url }, [])).toEqual({ ok: false, error: 'mcpUrlRequired' })
    expect(parseServerForm({ ...http, url: ' http://127.0.0.1:9000/mcp ', tools: ['a', 'a'] }, []))
      .toEqual({ ok: true, server: { name: 'a', enabled: true, transport: 'streamable-http', url: 'http://127.0.0.1:9000/mcp', tools: ['a'] } })
  })
  it('round-trips a saved stdio server and omits an empty environment', () => {
    expect(parseServerForm(serverFormOf(files), ['other'])).toEqual({ ok: true, server: files })
    expect(parseServerForm({ ...emptyServerForm(), name: 'a', command: 'run', env: '\n' }, []))
      .toEqual({ ok: true, server: { name: 'a', enabled: true, transport: 'stdio', command: 'run', args: [], tools: [] } })
    expect(parseServerForm({ ...emptyServerForm(), name: 'a', command: 'run', env: '1BAD=x' }, [])).toEqual({ ok: false, error: 'mcpEnvInvalid', vars: { line: 1 } })
    expect(parseServerForm({ ...emptyServerForm(), name: 'other', command: 'run' }, ['other'])).toEqual({ ok: false, error: 'mcpNameTaken', vars: { name: 'other' } })
  })
})
