// @vitest-environment happy-dom
/** Skills & MCP lists discovered skills and configured servers, and validates the server form before the Host sees it. */
import { act } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ExtensionsStatus, McpServerConfig } from '../../src/shared/rpc.ts'
import { ExtensionsSection } from '../../src/renderer/settings/ExtensionsSection.tsx'
import { emptyServerForm, isWindowsMcp, parseServerForm, serverFormOf, windowsMcpForm } from '../../src/renderer/settings/mcp-form.ts'
import { settingsMessages } from '../../src/renderer/settings/messages.ts'
import {
  all, button, cleanup, click, control, deferred, emit, fakeHost, flush, handle, hasText, listenerCount, render, toast, type, waitFor,
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
  await emit('prefs.changed', { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true, webSearch: true, markdownFocus: false, markdownTypewriter: false, markdownImages: 'assets' })
})
afterEach(async () => { await cleanup(); delete window.__RAINY_AGENT__ })

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

it('keeps a server switched off while its form was open switched off when the form is saved', async () => {
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(row('files')).not.toBeNull() })
  await click(button(zh.mcpEdit, row('files')))
  await click(button('启用 files'))
  await waitFor(() => { expect(button('启用 files').getAttribute('aria-checked')).toBe('false') })
  await type(control(zh.mcpArgs), '-y\n@mcp/files\n--verbose')
  await click(button(zh.settingsSave, form()))
  await waitFor(() => { expect(calls('extensions.saveServer')).toHaveLength(2) })
  expect(calls('extensions.saveServer')[1]).toEqual({ previousName: 'files', server: { ...files, enabled: false, args: ['-y', '@mcp/files', '--verbose'] } })
  await waitFor(() => { expect(button('启用 files').getAttribute('aria-checked')).toBe('false') })
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

const desktopArgs = ['--python', '3.14', '--exclude-newer', '2026-10-04T00:00:00Z', 'windows-mcp==0.8.7', 'serve', '--exclude-tools',
  'PowerShell,FileSystem,Registry,Process,Scrape,Notification']

it('adds computer use from a filled-in Windows-MCP form that starts it through interop on a WSL Host', async () => {
  window.__RAINY_AGENT__ = { name: 'Rainy', version: '3.6.2', environment: 'WSL' }
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(row('files')).not.toBeNull() })
  await click(button(zh.mcpAddDesktop))
  expect(hasText(zh.mcpFormAdd, form())).toBe(true)
  expect(form().querySelector('[data-desktop-note]')?.textContent).toBe(`${zh.mcpDesktopNote}${zh.mcpDesktopSetup}`)
  expect(control<HTMLInputElement>(zh.mcpName).value).toBe('windows')
  expect(control<HTMLInputElement>(zh.mcpCommand).value).toBe('cmd.exe')
  expect(control<HTMLTextAreaElement>(zh.mcpArgs).value).toBe(['/c', 'uvx', ...desktopArgs].join('\n'))
  expect(control<HTMLTextAreaElement>(zh.mcpEnv).value).toBe('ANONYMIZED_TELEMETRY=false\nWSLENV=ANONYMIZED_TELEMETRY')
  await click(button(zh.settingsSave, form()))
  await waitFor(() => { expect(row('windows')).not.toBeNull() })
  expect(calls('extensions.saveServer')).toEqual([{ server: { name: 'windows', enabled: true, transport: 'stdio', command: 'cmd.exe',
    args: ['/c', 'uvx', ...desktopArgs], env: { ANONYMIZED_TELEMETRY: 'false', WSLENV: 'ANONYMIZED_TELEMETRY' }, tools: [] } }])
  // Once added, the button goes away, and editing the server explains it again.
  expect(all('[data-add-desktop]')).toEqual([])
  await click(button(zh.mcpEdit, row('windows')))
  expect(form().querySelector('[data-desktop-note]')).not.toBeNull()
  await click(button(zh.mcpEdit, row('files')))
  expect(form().querySelector('[data-desktop-note]')).toBeNull()
})

it('offers computer use only where the Host can reach this PC\'s desktop', async () => {
  window.__RAINY_AGENT__ = { name: 'Rainy', version: '3.6.2', environment: 'SSH' }
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(row('files')).not.toBeNull() })
  expect(all('[data-add-desktop]')).toEqual([])
  await cleanup()
  window.__RAINY_AGENT__ = { name: 'Rainy', version: '3.6.2', environment: 'Windows' }
  handle('extensions.status', () => status)
  await render(<ExtensionsSection workspace={workspace} />)
  await waitFor(() => { expect(all('[data-add-desktop]')).toHaveLength(1) })
})

describe('importing MCP JSON', () => {
  const panel = (): HTMLElement => document.querySelector<HTMLElement>('[data-mcp-import]')!
  const imported = (name: string): HTMLElement => panel().querySelector<HTMLElement>(`[data-import-server="${name}"]`)!
  const readme = JSON.stringify({ mcpServers: {
    files: { command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem', '/tmp'] },
    github: { command: 'npx', args: ['-y', '@modelcontextprotocol/server-github'], env: { GITHUB_PERSONAL_ACCESS_TOKEN: '<YOUR_TOKEN>' } },
    remote: { url: 'https://mcp.example.com/mcp?api_key=secret-1', headers: { Authorization: 'Bearer real' } },
    broken: { url: 'ftp://nowhere' },
  } })
  const paste = async (target: HTMLElement, text: string): Promise<void> => {
    const event = new Event('paste', { bubbles: true, cancelable: true })
    Object.defineProperty(event, 'clipboardData', { value: { getData: () => text } })
    await act(async () => { target.dispatchEvent(event) })
  }

  it('previews pasted servers with new names, notes and masked secrets, and imports the checked ones', async () => {
    await render(<ExtensionsSection workspace={workspace} />)
    await waitFor(() => { expect(row('files')).not.toBeNull() })
    await click(button(zh.mcpImport))
    expect(button(`导入 0 个服务器`, panel()).disabled).toBe(true)
    await type(control<HTMLTextAreaElement>(zh.mcpImportLabel), readme)
    expect(hasText('找到 3 个服务器，勾选要导入的：', panel())).toBe(true)
    expect(imported('files-2').textContent).toContain('名称 files 改为 files-2')
    expect(imported('github').textContent).toContain('GITHUB_PERSONAL_ACCESS_TOKEN 里的 <YOUR_TOKEN> 是占位符')
    expect(imported('github').textContent).toContain('环境变量：GITHUB_PERSONAL_ACCESS_TOKEN')
    expect(imported('github').textContent).not.toContain('<YOUR_TOKEN>stdio')
    expect(imported('remote').textContent).toContain('https://mcp.example.com/mcp?api_key=•••')
    expect(imported('remote').textContent).toContain('请求头：Authorization')
    const shown = all('[data-import-server]', panel()).map(item => item.textContent).join(' ')
    expect(shown).not.toContain('secret-1')
    expect(shown).not.toContain('Bearer real')
    expect(panel().querySelector('[data-import-problems]')?.textContent).toBe(`${zh.mcpImportSkipped}broken：URL 需要以 http:// 或 https:// 开头`)
    await click(imported('files-2').querySelector<HTMLInputElement>('input[type="checkbox"]')!)
    await click(button('导入 2 个服务器', panel()))
    await waitFor(() => { expect(document.querySelector('[data-mcp-import]')).toBeNull() })
    expect(calls('extensions.saveServer')).toEqual([
      { server: { name: 'github', enabled: true, transport: 'stdio', command: 'npx', args: ['-y', '@modelcontextprotocol/server-github'],
        env: { GITHUB_PERSONAL_ACCESS_TOKEN: '<YOUR_TOKEN>' }, tools: [] } },
      { server: { name: 'remote', enabled: true, transport: 'streamable-http', url: 'https://mcp.example.com/mcp?api_key=secret-1',
        headers: { Authorization: 'Bearer real' }, tools: [] } },
    ])
    expect(row('github')).not.toBeNull()
    expect(row('remote')).not.toBeNull()
    expect(document.querySelector('[data-server="files-2"]')).toBeNull()
    expect(toast).toHaveBeenCalledWith('已导入 2 个服务器', { tone: 'success' })
  })

  it('says why text cannot be imported and keeps the import button off', async () => {
    await render(<ExtensionsSection workspace={workspace} />)
    await waitFor(() => { expect(row('files')).not.toBeNull() })
    await click(button(zh.mcpImport))
    await type(control<HTMLTextAreaElement>(zh.mcpImportLabel), '[mcp_servers.github]\ncommand = "npx"')
    expect(panel().querySelector('[role="alert"]')?.textContent).toBe(zh.mcpJsonToml)
    expect(button('导入 0 个服务器', panel()).disabled).toBe(true)
    await type(control<HTMLTextAreaElement>(zh.mcpImportLabel), '')
    expect(panel().querySelector('[role="alert"]')).toBeNull()
    await click(button(zh.settingsCancel, panel()))
    expect(document.querySelector('[data-mcp-import]')).toBeNull()
    expect(calls('extensions.saveServer')).toEqual([])
  })

  it('turns MCP JSON pasted into a new server form into an import, but not into an edited server', async () => {
    await render(<ExtensionsSection workspace={workspace} />)
    await waitFor(() => { expect(row('files')).not.toBeNull() })
    await click(button(zh.mcpEdit, row('files')))
    await paste(control(zh.mcpCommand), readme)
    expect(document.querySelector('[data-mcp-import]')).toBeNull()
    await click(button(zh.mcpAdd))
    await paste(control(zh.mcpCommand), 'npx -y @modelcontextprotocol/server-github')
    expect(document.querySelector('[data-mcp-import]')).toBeNull()
    await paste(control(zh.mcpName), readme)
    await flush()
    expect(document.querySelector('[data-server-form]')).toBeNull()
    expect(control<HTMLTextAreaElement>(zh.mcpImportLabel).value).toBe(readme)
    expect(imported('github')).not.toBeNull()
  })

  it('leaves out the servers already saved when one import fails, so trying again adds no copies', async () => {
    handle('extensions.saveServer', ({ server }) => {
      if (server.name === 'remote') throw new Error('Host refused')
      status = { ...status, skills: [], servers: [...status.servers, server] }
      return status
    })
    await render(<ExtensionsSection workspace={workspace} />)
    await waitFor(() => { expect(row('files')).not.toBeNull() })
    await click(button(zh.mcpImport))
    await type(control<HTMLTextAreaElement>(zh.mcpImportLabel), readme)
    await click(button('导入 3 个服务器', panel()))
    await waitFor(() => { expect(toast).toHaveBeenCalledWith('导入 remote 时出错：Host refused。在它之前的 2 个已导入。', { tone: 'error' }) })
    expect(calls('extensions.saveServer').map(params => (params as { server: McpServerConfig }).server.name)).toEqual(['files-2', 'github', 'remote'])
    expect(button('导入 1 个服务器', panel()).disabled).toBe(false)
    expect(row('files-2')).not.toBeNull()
    expect(row('github')).not.toBeNull()
    // Editing the text afterwards still leaves the saved servers out: they now match configured ones.
    await type(control<HTMLTextAreaElement>(zh.mcpImportLabel), `${readme}\n`)
    expect(all('[data-import-server]', panel()).map(item => item.getAttribute('data-import-server'))).toEqual(['remote'])
    expect(panel().querySelector('[data-import-problems]')?.textContent).toContain('files：和已有的服务器 files-2 完全相同，不再重复添加')
  })

  it('locks the panel while the import runs, so what is saved is what is shown', async () => {
    const saving = deferred<ExtensionsStatus>()
    handle('extensions.saveServer', () => saving.promise)
    await render(<ExtensionsSection workspace={workspace} />)
    await waitFor(() => { expect(row('files')).not.toBeNull() })
    await click(button(zh.mcpImport))
    await type(control<HTMLTextAreaElement>(zh.mcpImportLabel), readme)
    await click(button('导入 3 个服务器', panel()))
    expect(control<HTMLTextAreaElement>(zh.mcpImportLabel).disabled).toBe(true)
    expect(all<HTMLInputElement>('input[type="checkbox"]', panel()).every(box => box.disabled)).toBe(true)
    expect(button(zh.mcpImportFile, panel()).disabled).toBe(true)
    await act(async () => { saving.reject(new Error('stop')) })
    await waitFor(() => { expect(control<HTMLTextAreaElement>(zh.mcpImportLabel).disabled).toBe(false) })
  })

  it('pastes into a new server form as usual once something is typed, or when the text is only a value', async () => {
    await render(<ExtensionsSection workspace={workspace} />)
    await waitFor(() => { expect(row('files')).not.toBeNull() })
    await click(button(zh.mcpAdd))
    await paste(control(zh.mcpEnv), '{"url": "https://api.example.com", "region": "eu"}')
    expect(document.querySelector('[data-server-form]')).not.toBeNull()
    await type(control(zh.mcpName), 'typed')
    await paste(control(zh.mcpEnv), readme)
    await flush()
    expect(document.querySelector('[data-server-form]')).not.toBeNull()
    expect(document.querySelector('[data-mcp-import]')).toBeNull()
    expect(control<HTMLInputElement>(zh.mcpName).value).toBe('typed')
  })

  it('drops a file read that finishes after the panel was left', async () => {
    const content = deferred<ArrayBuffer>()
    await render(<ExtensionsSection workspace={workspace} />)
    await waitFor(() => { expect(row('files')).not.toBeNull() })
    await click(button(zh.mcpImport))
    const input = panel().querySelector<HTMLInputElement>('input[type="file"]')!
    Object.defineProperty(input, 'files', { configurable: true, value: [{ size: 100, arrayBuffer: () => content.promise }] })
    await act(async () => { input.dispatchEvent(new Event('change', { bubbles: true })) })
    await click(button(zh.mcpAdd))
    await act(async () => { content.resolve(new TextEncoder().encode(readme).buffer as ArrayBuffer) })
    await flush()
    expect(document.querySelector('[data-server-form]')).not.toBeNull()
    expect(document.querySelector('[data-mcp-import]')).toBeNull()
  })

  it('reads a chosen file into the text box', async () => {
    await render(<ExtensionsSection workspace={workspace} />)
    await waitFor(() => { expect(row('files')).not.toBeNull() })
    await click(button(zh.mcpImport))
    const input = panel().querySelector<HTMLInputElement>('input[type="file"]')!
    Object.defineProperty(input, 'files', { configurable: true, value: [new File([readme], 'claude_desktop_config.json', { type: 'application/json' })] })
    await act(async () => { input.dispatchEvent(new Event('change', { bubbles: true })) })
    await waitFor(() => { expect(control<HTMLTextAreaElement>(zh.mcpImportLabel).value).toBe(readme) })
    expect(imported('remote')).not.toBeNull()
  })
})

describe('windowsMcpForm', () => {
  it('runs the pinned release through uvx on Windows and picks a free name', () => {
    const preset = windowsMcpForm('Windows', ['windows', 'windows-2'])
    expect(preset).toMatchObject({ name: 'windows-3', transport: 'stdio', command: 'uvx', args: desktopArgs.join('\n'), env: 'ANONYMIZED_TELEMETRY=false' })
    const parsed = parseServerForm(preset, ['windows', 'windows-2'])
    expect(parsed.ok && isWindowsMcp(parsed.server)).toBe(true)
  })
  it('recognizes Windows-MCP servers configured by hand', () => {
    const server = (command: string, args: string[]): McpServerConfig => ({ name: 'w', enabled: true, transport: 'stdio', command, args, tools: [] })
    expect(isWindowsMcp(server('uvx', ['windows-mcp', 'serve']))).toBe(true)
    expect(isWindowsMcp(server('C:\\Users\\u\\.local\\bin\\uvx.exe', ['windows-mcp@latest']))).toBe(true)
    expect(isWindowsMcp(server('windows-mcp', ['serve']))).toBe(true)
    expect(isWindowsMcp(server('npx', ['-y', 'windows-mcp-helper']))).toBe(false)
    expect(isWindowsMcp(files)).toBe(false)
  })
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
  it('reads HTTP request headers one per line and round-trips them', () => {
    const http = { ...emptyServerForm(), name: 'web', transport: 'streamable-http' as const, url: 'https://mcp.example.test/mcp' }
    const parsed = parseServerForm({ ...http, headers: 'Authorization: Bearer a:b \n\nX-Team:rainy' }, [])
    expect(parsed).toEqual({ ok: true, server: { name: 'web', enabled: true, transport: 'streamable-http', url: 'https://mcp.example.test/mcp',
      headers: { Authorization: 'Bearer a:b', 'X-Team': 'rainy' }, tools: [] } })
    if (parsed.ok) expect(parseServerForm(serverFormOf(parsed.server), [])).toEqual(parsed)
    expect(parseServerForm({ ...http, headers: '\n' }, [])).toEqual({ ok: true, server: { name: 'web', enabled: true, transport: 'streamable-http',
      url: 'https://mcp.example.test/mcp', tools: [] } })
    expect(parseServerForm({ ...http, headers: 'X-Ok: 1\nbad header: x' }, [])).toEqual({ ok: false, error: 'mcpHeaderInvalid', vars: { line: 2 } })
    expect(parseServerForm({ ...http, headers: 'NoColon' }, [])).toEqual({ ok: false, error: 'mcpHeaderInvalid', vars: { line: 1 } })
    expect(parseServerForm({ ...http, headers: 'X-Key: 1\nx-key: 2' }, [])).toEqual({ ok: false, error: 'mcpHeaderDuplicate', vars: { name: 'x-key' } })
  })
})
