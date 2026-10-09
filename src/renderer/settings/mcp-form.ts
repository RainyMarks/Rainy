/** Text form of an MCP server in Settings → Skills & MCP, and its conversion to `McpServerConfig`. */
import type { McpServerConfig } from '../../shared/rpc.ts'
import type { SettingsMessage } from './messages.ts'

/** Unsaved server fields: arguments and environment as one entry per line. */
export interface McpServerForm {
  name: string
  enabled: boolean
  transport: McpServerConfig['transport']
  command: string
  args: string
  env: string
  url: string
  tools: string[]
}

/** Server names the Host accepts. */
export const MCP_SERVER_NAME = /^[A-Za-z0-9_-]{1,32}$/
const ENV_LINE = /^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/

/** @returns The form of a new, enabled stdio server. */
export function emptyServerForm(): McpServerForm {
  return { name: '', enabled: true, transport: 'stdio', command: '', args: '', env: '', url: '', tools: [] }
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
    url: server.url ?? '', tools: [...server.tools],
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
    return { ok: true, server: { name, enabled: form.enabled, transport: 'streamable-http', url, tools } }
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
