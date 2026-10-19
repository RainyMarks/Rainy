/** Text form of an MCP server in Settings → Skills & MCP, and its conversion to `McpServerConfig`. */
import type { McpServerConfig } from '../../shared/rpc.ts'
import type { SettingsMessage } from './messages.ts'

/** Unsaved server fields: arguments, environment and headers as one entry per line. */
export interface McpServerForm {
  name: string
  enabled: boolean
  transport: McpServerConfig['transport']
  command: string
  args: string
  env: string
  url: string
  headers: string
  tools: string[]
}

/** Server names the Host accepts. */
export const MCP_SERVER_NAME = /^[A-Za-z0-9_-]{1,32}$/
const ENV_LINE = /^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/
/** An RFC 9110 field name, a colon and a value without control characters other than tab. */
const HEADER_LINE = /^([!#$%&'*+.^_`|~0-9A-Za-z-]+):[ \t]*([^\x00-\x08\x0a-\x1f\x7f]*)$/

/** @returns The form of a new, enabled stdio server. */
export function emptyServerForm(): McpServerForm {
  return { name: '', enabled: true, transport: 'stdio', command: '', args: '', env: '', url: '', headers: '', tools: [] }
}

/** Windows-MCP (github.com/CursorTouch/Windows-MCP, MIT) release the computer-use preset runs. */
export const WINDOWS_MCP_VERSION = '0.8.7'
/** Its dependencies resolve as published before this moment, so a new upload cannot change what runs. */
const WINDOWS_MCP_DEPENDENCIES_BEFORE = '2026-10-04T00:00:00Z'
/** Its tools that Rainy's own command, file and web tools cover with their own approvals, or that a chat has no use for. */
const WINDOWS_MCP_EXCLUDED_TOOLS = ['PowerShell', 'FileSystem', 'Registry', 'Process', 'Scrape', 'Notification']

/**
 * @param server A configured server.
 * @returns Whether it runs Windows-MCP.
 */
export function isWindowsMcp(server: McpServerConfig): boolean {
  return server.transport === 'stdio' && [server.command ?? '', ...server.args ?? []].some(part => /^windows-mcp(?:[=<>@~!]|$)/iu.test(part))
}

/**
 * The form of the computer-use preset: Windows-MCP through uv, which downloads the Python it needs.
 * @param environment Where the Host runs; a WSL Host starts the Windows program through interop.
 * @param takenNames Names of the configured servers.
 * @returns A new, enabled stdio server form.
 */
export function windowsMcpForm(environment: 'Windows' | 'WSL', takenNames: readonly string[]): McpServerForm {
  const uvx = ['uvx', '--python', '3.14', '--exclude-newer', WINDOWS_MCP_DEPENDENCIES_BEFORE, `windows-mcp==${WINDOWS_MCP_VERSION}`,
    'serve', '--exclude-tools', WINDOWS_MCP_EXCLUDED_TOOLS.join(',')]
  const [command, ...args] = environment === 'WSL' ? ['cmd.exe', '/c', ...uvx] : uvx
  let name = 'windows'
  for (let index = 2; takenNames.includes(name); index++) name = `windows-${index}`
  return {
    ...emptyServerForm(), name, command: command!, args: args.join('\n'),
    // WSLENV carries the variable from the Linux Host to the Windows program.
    env: environment === 'WSL' ? 'ANONYMIZED_TELEMETRY=false\nWSLENV=ANONYMIZED_TELEMETRY' : 'ANONYMIZED_TELEMETRY=false',
  }
}

/**
 * @param server A saved server.
 * @returns Its editable form.
 */
export function serverFormOf(server: McpServerConfig): McpServerForm {
  return {
    name: server.name, enabled: server.enabled, transport: server.transport, command: server.command ?? '',
    args: (server.args ?? []).join('\n'),
    env: Object.entries(server.env ?? {}).map(([key, value]) => `${key}=${value}`).join('\n'),
    url: server.url ?? '',
    headers: Object.entries(server.headers ?? {}).map(([key, value]) => `${key}: ${value}`).join('\n'),
    tools: [...server.tools],
  }
}

/** A valid server, or the message key and values describing the first invalid field. */
export type McpServerFormResult =
  | { ok: true; server: McpServerConfig }
  | { ok: false; error: SettingsMessage; vars?: Record<string, string | number> }

const lines = (text: string): string[] => text.split('\n').map(line => line.trim())

/**
 * Validate a server form.
 * @param form The form.
 * @param otherNames Names of the other configured servers.
 * @returns The server to save, or the reason it cannot be saved.
 */
export function parseServerForm(form: McpServerForm, otherNames: readonly string[]): McpServerFormResult {
  const name = form.name.trim()
  if (!MCP_SERVER_NAME.test(name)) return { ok: false, error: 'mcpNameInvalid' }
  if (otherNames.includes(name)) return { ok: false, error: 'mcpNameTaken', vars: { name } }
  const tools = [...new Set(form.tools)]
  if (form.transport === 'streamable-http') {
    const url = form.url.trim()
    let parsed: URL | undefined
    try { parsed = new URL(url) } catch (_invalid) { parsed = undefined }
    if (parsed === undefined || (parsed.protocol !== 'http:' && parsed.protocol !== 'https:')) return { ok: false, error: 'mcpUrlRequired' }
    const headers: [string, string][] = []
    for (const [index, line] of lines(form.headers).entries()) {
      if (line === '') continue
      const match = HEADER_LINE.exec(line)
      if (match === null) return { ok: false, error: 'mcpHeaderInvalid', vars: { line: index + 1 } }
      const key = match[1]!
      if (headers.some(([other]) => other.toLowerCase() === key.toLowerCase())) return { ok: false, error: 'mcpHeaderDuplicate', vars: { name: key } }
      headers.push([key, match[2]!.trimEnd()])
    }
    // Object.fromEntries defines every name as an own property, `__proto__` included.
    return { ok: true, server: { name, enabled: form.enabled, transport: 'streamable-http', url,
      ...headers.length === 0 ? {} : { headers: Object.fromEntries(headers) }, tools } }
  }
  const command = form.command.trim()
  if (command === '') return { ok: false, error: 'mcpCommandRequired' }
  const env: Record<string, string> = {}
  const envLines = lines(form.env)
  for (const [index, line] of envLines.entries()) {
    if (line === '') continue
    const match = ENV_LINE.exec(line)
    if (match === null) return { ok: false, error: 'mcpEnvInvalid', vars: { line: index + 1 } }
    env[match[1]!] = match[2]!
  }
  const args = lines(form.args).filter(line => line !== '')
  return { ok: true, server: { name, enabled: form.enabled, transport: 'stdio', command, args,
    ...Object.keys(env).length === 0 ? {} : { env }, tools } }
}
