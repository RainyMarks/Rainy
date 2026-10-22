/**
 * MCP servers from pasted JSON: the `mcpServers` files of Claude Desktop, Claude Code, Cursor, Windsurf, Cline, Roo Code,
 * Cherry Studio, Trae, Kiro and Gemini CLI, VS Code's `servers`, Zed's `context_servers`, opencode's `mcp`, Continue's
 * server list and the snippets MCP server READMEs show, read into Rainy's own servers.
 */
import type { McpServerConfig } from '../../shared/rpc.ts'
import { MCP_SERVER_NAME } from './mcp-form.ts'
import type { SettingsMessage } from './messages.ts'

/** Longest text read, in characters. */
export const MCP_JSON_MAX_LENGTH = 512 * 1024
/** Most servers one import adds. */
export const MCP_JSON_MAX_SERVERS = 64
/** Most code blocks, or `{…}` blocks in prose, read from one text. */
const MAX_BLOCKS = 64

/** A message key with its values. */
export interface McpImportMessage {
  readonly key: SettingsMessage
  readonly vars?: Record<string, string | number> | undefined
}

/** One server read from the text. */
export interface ImportedServer {
  /** The name the text gave it; empty when it gave none. */
  readonly source: string
  /** What is saved; its name is cleaned up and kept clear of the configured servers. */
  readonly server: McpServerConfig
  /** What the user should know: a new name, an ignored field, a placeholder still to fill in. */
  readonly notes: readonly McpImportMessage[]
}

/** The servers read with what was noticed about the whole text, or why the text holds no servers at all. */
export type McpImportResult =
  | {
    readonly ok: true
    readonly servers: readonly ImportedServer[]
    /** Entries that could not be read, each with its reason. */
    readonly problems: readonly McpImportMessage[]
    /** Repairs made to the text itself. */
    readonly notices: readonly McpImportMessage[]
  }
  | { readonly ok: false; readonly error: McpImportMessage }

/** Where the Host runs the servers' commands. */
export type McpImportEnvironment = 'Windows' | 'WSL' | 'SSH'

type Json = null | boolean | number | string | Json[] | { [key: string]: Json }
type JsonObject = { [key: string]: Json }
type ServerFields = Omit<McpServerConfig, 'name'>

const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]*$/
/** An RFC 9110 field name. */
const HEADER_NAME = /^[!#$%&'*+.^_`|~0-9A-Za-z-]+$/
/** A value Node's `validateHeaderValue` accepts, as the Host checks: tab, visible ASCII and Latin-1, nothing beyond. */
const HEADER_VALUE = /^[\t\x20-\x7e\x80-\xff]*$/
/**
 * Values left for the user or for the client to fill in, which Rainy passes on unchanged: `${input:token}` (VS Code),
 * `${env:NAME}` (Cursor, Windsurf), `${NAME}` and `${NAME:-default}` (Claude Code), `{env:NAME}` (opencode), `$NAME`
 * (Gemini CLI), `${{ secrets.NAME }}` (Continue), `<YOUR_TOKEN>`, `YOUR_API_KEY`, `your-key-here`, `sk-xxx`.
 */
const PLACEHOLDER = new RegExp([
  String.raw`\$\{\{[^}]{0,80}\}\}`,
  String.raw`\$\{[^}]{1,80}\}`,
  String.raw`\{(?:env|file):[^}]{1,80}\}`,
  String.raw`\$[A-Z_][A-Z0-9_]{2,64}\b`,
  String.raw`<[^<>\s]{2,64}>`,
  String.raw`\bYOUR[_-][A-Z0-9_-]{1,64}`,
  String.raw`\byour[-_][a-z0-9_-]{0,64}(?:key|token|here|secret|password)\b`,
  String.raw`\b(?:sk-)?x{3,64}\b`,
].join('|'), 'u')

function isObject(value: Json | undefined): value is JsonObject {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

/** Drop `//` and `/* *\/` comments outside strings (VS Code's JSONC). */
function stripComments(text: string): string {
  let out = ''
  let index = 0
  let inString = false
  while (index < text.length) {
    const char = text[index]!
    if (inString) {
      out += char
      if (char === '\\') { out += text[index + 1] ?? ''; index += 2; continue }
      if (char === '"') inString = false
      index++
      continue
    }
    if (char === '"') { inString = true; out += char; index++; continue }
    if (char === '/' && text[index + 1] === '/') {
      while (index < text.length && text[index] !== '\n') index++
      continue
    }
    if (char === '/' && text[index + 1] === '*') {
      const end = text.indexOf('*/', index + 2)
      index = end < 0 ? text.length : end + 2
      continue
    }
    out += char
    index++
  }
  return out
}

/** Drop commas outside strings that close nothing: before `}`, `]` or the end. */
function stripTrailingCommas(text: string): string {
  let out = ''
  let inString = false
  for (let index = 0; index < text.length; index++) {
    const char = text[index]!
    if (inString) {
      out += char
      if (char === '\\') { out += text[index + 1] ?? ''; index++; continue }
      if (char === '"') inString = false
      continue
    }
    if (char === '"') { inString = true; out += char; continue }
    if (char === ',') {
      let next = index + 1
      while (next < text.length && /\s/u.test(text[next]!)) next++
      if (next >= text.length || text[next] === '}' || text[next] === ']') continue
    }
    out += char
  }
  return out
}

/** Straighten what web pages and chat apps do to copied JSON: curly quotes, full-width punctuation, invisible spaces. */
function straighten(text: string): string {
  return text.replace(/[\u200B-\u200D\u2060\uFEFF]/gu, '').replace(/[\u00A0\u3000]/gu, ' ')
    .replace(/[“”„‟″＂]/gu, '"').replace(/：/gu, ':').replace(/，/gu, ',')
    .replace(/｛/gu, '{').replace(/｝/gu, '}').replace(/［/gu, '[').replace(/］/gu, ']')
}

/**
 * Double the backslashes of Windows paths written with single ones (`"C:\Users\me\new\server.py"`, `.\build\index.js`),
 * which JSON reads as escapes. Only tried once the text failed to parse: MCP values hold no tabs or line breaks, so `\n`,
 * `\t`, `\b`, `\f` and `\r` are taken as folder names; `\\`, `\"`, `\/` and `\uXXXX` stay escapes.
 */
function doubleBackslashes(text: string): string {
  // A path that ends in a backslash, `"D:\Projects\"`, would otherwise escape its own closing quote.
  const closed = text.replace(/(?<!\\)\\"(?=[ \t]*[,\]}\r\n])/gu, '\\\\"')
  // One pass, string by string: a string runs to the next unescaped quote on its line; one left open is copied as it is.
  let out = ''
  let index = 0
  while (index < closed.length) {
    const quote = closed.indexOf('"', index)
    if (quote < 0) { out += closed.slice(index); break }
    out += closed.slice(index, quote + 1)
    let end = quote + 1
    let body = ''
    while (end < closed.length && closed[end] !== '"' && closed[end] !== '\n') {
      const char = closed[end]!
      if (char !== '\\') { body += char; end++; continue }
      const next = closed[end + 1] ?? ''
      const escape = next === '"' || next === '\\' || next === '/' ? 2 : next === 'u' && /^[0-9A-Fa-f]{4}$/u.test(closed.slice(end + 2, end + 6)) ? 6 : 0
      body += escape === 0 ? '\\\\' : closed.slice(end, end + escape)
      end += escape === 0 ? 1 : escape
    }
    if (closed[end] === '"') { out += `${body}"`; index = end + 1 } else { out += closed.slice(quote + 1, end); index = end }
  }
  return out
}

function tryParse(text: string): Json | undefined {
  try { return JSON.parse(text) as Json } catch (_invalid) { return undefined }
}

function parseError(text: string): string {
  try { JSON.parse(text); return '' } catch (error) {
    // V8 quotes the text around the error, which may be part of a token; the rest of its message says what went wrong.
    return (error instanceof Error ? error.message : String(error)).replace(/,?\s*(?:\.\.\.)?"[\s\S]*"(?:\.\.\.)? is not valid JSON$/u, '').trim()
  }
}

/**
 * Read one block as JSON, forgiving what copying commonly does to it: comments, trailing commas, a fragment such as
 * `"github": { … }` without its outer braces, typography from web pages, and single backslashes in Windows paths.
 */
function readBlock(text: string): { value: Json; backslashes: boolean } | { error: string } {
  const prepare = (body: string): string => stripTrailingCommas(stripComments(body.trim()))
  const cleaned = prepare(text)
  const wrap = (body: string): string[] => /^[[{]/u.test(body) ? [body] : [body, `{${body}}`]
  // The repairs that change values are tried last, and typography last of all: it also rewrites quotes inside values.
  const attempts: [string, boolean][] = [
    ...wrap(cleaned).map((body): [string, boolean] => [body, false]),
    ...wrap(prepare(doubleBackslashes(text))).map((body): [string, boolean] => [body, true]),
    ...wrap(prepare(straighten(text))).map((body): [string, boolean] => [body, false]),
    ...wrap(prepare(doubleBackslashes(straighten(text)))).map((body): [string, boolean] => [body, true]),
  ]
  for (const [attempt, backslashes] of attempts) {
    const value = tryParse(attempt)
    if (value !== undefined) return { value, backslashes }
  }
  return { error: parseError(cleaned) }
}

/**
 * The balanced `{…}` blocks of a text, outside strings, skipping the prose between them. Lists are skipped too: in prose
 * or TOML they are arguments, not configurations.
 */
function topLevelBlocks(text: string): string[] {
  const blocks: string[] = []
  let depth = 0
  let start = -1
  let inString = false
  for (let index = 0; index < text.length; index++) {
    const char = text[index]!
    if (inString) {
      if (char === '\\') index++
      else if (char === '"') inString = false
      continue
    }
    if (char === '"' && depth > 0) inString = true
    else if (char === '{' || char === '[') { if (depth++ === 0) start = index }
    else if ((char === '}' || char === ']') && depth > 0 && --depth === 0 && text[start] === '{') blocks.push(text.slice(start, index + 1))
  }
  return blocks
}

/** The JSON values of the text: its Markdown code blocks and what lies outside them, each whole or block by block. */
function readValues(text: string): { values: Json[]; backslashes: boolean } | { error: string } {
  // The info string holds no backticks, so a long run of them is not scanned again from every position.
  const fences = [...text.matchAll(/```[^\n`]*\n([\s\S]*?)```/gu)]
  // Only blocks with a quote can hold a server; a paste has a handful of them, so a flood of empty ones is cut short.
  const sources = fences.map(match => match[1]!).filter(source => /["“”＂]/u.test(source)).slice(0, MAX_BLOCKS)
  // Outside the code blocks: the whole paste when it has none, else JSON written after an install command and the like.
  let outside = ''
  let end = 0
  for (const match of fences) { outside += `${text.slice(end, match.index)}\n`; end = match.index + match[0].length }
  outside += text.slice(end)
  if (fences.length === 0 || /[{[]/u.test(outside)) sources.push(outside)
  const values: Json[] = []
  let backslashes = false
  let first: string | undefined
  const take = (read: ReturnType<typeof readBlock>): boolean => {
    if (!('value' in read)) return false
    values.push(read.value)
    backslashes ||= read.backslashes
    return true
  }
  for (const source of sources) {
    const whole = readBlock(source)
    if (take(whole)) continue
    if ('error' in whole) first ??= whole.error
    // Prose around the JSON, or several blocks pasted one after another; full-width braces only when nothing else reads.
    const blocks = (body: string): string[] => topLevelBlocks(body).filter(block => /["“”＂]/u.test(block)).slice(0, MAX_BLOCKS)
    const found = blocks(source).map(block => take(readBlock(block))).filter(Boolean).length
    if (found === 0) for (const block of blocks(straighten(source))) take(readBlock(block))
  }
  return values.length > 0 ? { values, backslashes } : { error: first ?? 'JSON.parse failed' }
}

/** Whether one object describes a single server rather than a set of them. */
function looksLikeServer(value: Json | undefined): value is JsonObject {
  if (!isObject(value)) return false
  return ['command', 'url', 'serverUrl', 'httpUrl', 'baseUrl'].some(key => key in value) || isObject(value.transport)
    || (typeof value.type === 'string' && /^(?:stdio|local|remote|http|sse|streamable[-_]?http)$/iu.test(value.type))
}

/** Whether every value of an object describes a server: a name-to-server map without its container key. */
function looksLikeServerMap(value: JsonObject): boolean {
  const values = Object.values(value)
  return values.length > 0 && values.every(looksLikeServer)
}

/** The `[name, server]` entries of one value, from whichever container it uses; unnamed entries get an empty name. */
function serverEntries(root: Json): [string, Json][] {
  const fromContainer = (value: Json | undefined): [string, Json][] => {
    if (Array.isArray(value)) return value.map((entry): [string, Json] => [isObject(entry) && typeof entry.name === 'string' ? entry.name : '', entry])
    if (isObject(value)) return Object.entries(value)
    return []
  }
  if (Array.isArray(root)) return fromContainer(root)
  if (!isObject(root)) return []
  // Most clients: `mcpServers` (an array in Continue); VS Code `.vscode/mcp.json`: `servers`; VS Code settings.json:
  // `mcp.servers`; Zed: `context_servers`; opencode: `mcp` whose values are servers (Gemini CLI's `mcp` is a name filter).
  const mcp = root.mcp
  const containers: (Json | undefined)[] = [
    root.mcpServers, root.mcp_servers, root.servers, isObject(mcp) ? mcp.servers : undefined, root.context_servers,
    isObject(mcp) && looksLikeServerMap(mcp) ? mcp : undefined,
    isObject(root.experimental) ? root.experimental.modelContextProtocolServers : undefined,
  ]
  const entries = containers.flatMap(fromContainer)
  // ~/.claude.json keeps servers per project next to the user's own.
  if (isObject(root.projects)) entries.push(...Object.values(root.projects).flatMap(project => isObject(project) ? fromContainer(project.mcpServers) : []))
  if (entries.length > 0) return entries
  if (looksLikeServer(root)) return [[typeof root.name === 'string' ? root.name : '', root]]
  if (looksLikeServerMap(root)) return Object.entries(root)
  return []
}

/** Text of a scalar written where text belongs; numbers and booleans are written out, anything else is refused. */
function scalar(value: Json | undefined): string | undefined {
  if (typeof value === 'string') return value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  return undefined
}

/** The first of some fields that holds non-empty text; Cherry Studio exports leave unused fields as `""`. */
function firstText(value: JsonObject, keys: readonly string[]): string | undefined {
  for (const key of keys) {
    const text = scalar(value[key])?.trim()
    if (text !== undefined && text !== '') return text
  }
  return undefined
}

/** A package, image or file name without its scope, extension and the MCP words that say nothing about the server. */
function bareName(part: string): string {
  return part.replace(/^@/u, '').replace(/\.(?:m?js|cjs|ts|py|exe|cmd|jar)$/iu, '')
    .replace(/^(?:mcp[-_]?server[-_]|server[-_]|mcp[-_])/iu, '').replace(/[-_](?:mcp[-_]?server|mcp|server)$/iu, '')
}

/** A name the Host accepts, built from what the text called the server: other characters become hyphens. */
export function cleanServerName(source: string, fallback: string): string {
  const trimmed = source.trim()
  if (MCP_SERVER_NAME.test(trimmed)) return trimmed
  let cleaned = trimmed.replace(/[^A-Za-z0-9_-]+/gu, '-').replace(/^[-_]+|[-_]+$/gu, '')
  if (cleaned.length > 32) {
    // `@modelcontextprotocol/server-github` is named after its package; `awslabs.aws-documentation-mcp-server` loses the MCP part.
    const last = trimmed.split(/[\\/]+/u).filter(part => part !== '').at(-1)
    if (last !== undefined && last !== trimmed) return cleanServerName(bareName(last), fallback)
    cleaned = cleaned.replace(/^mcp[-_]server[-_]|[-_]mcp(?:[-_]server)?$/iu, '')
  }
  cleaned = cleaned.slice(0, 32).replace(/[-_]+$/u, '')
  return MCP_SERVER_NAME.test(cleaned) ? cleaned : fallback
}

/** Path parts that say nothing about which server it is. */
const GENERIC_PARTS = new Set(['index', 'main', 'server', 'cli', 'dist', 'build', 'src', 'bin', 'lib', 'mcp', 'app', 'run', 'latest'])

/**
 * A name for a server the text left unnamed: the package, image or script it runs, or the site of its URL.
 * `npx -y @modelcontextprotocol/server-github` gives `github`, `@playwright/mcp@latest` gives `playwright`.
 */
export function unnamedServerHint(server: ServerFields): string {
  if (server.transport !== 'stdio') {
    let hostname: string
    try { hostname = new URL(server.url ?? '').hostname } catch (_invalid) { return '' }
    if (/^[\d.]+$|:/u.test(hostname)) return ''
    const labels = hostname.split('.').filter(label => !/^(?:www|api|mcp)$/iu.test(label))
    return labels.length >= 2 ? labels[labels.length - 2]! : labels[0] ?? ''
  }
  const words = [server.command ?? '', ...server.args ?? []].filter(word => word !== '' && !word.startsWith('-'))
  let pick = words.find(word => /mcp/iu.test(word)) ?? words[1] ?? words[0] ?? ''
  // `pkg@latest`, `pkg==1.2`: the version says nothing about the server.
  const at = pick.lastIndexOf('@')
  if (at > 0 && !/[\\/]/u.test(pick.slice(at))) pick = pick.slice(0, at)
  pick = pick.replace(/(?:==|>=|<=|~=|!=).*$/u, '')
  const parts = pick.split(/[\\/]+/u).map(bareName).filter(part => part !== '' && !GENERIC_PARTS.has(part.toLowerCase()))
  return parts.at(-1) ?? ''
}

/** `name`, or `name-2`, `name-3`… — the first one not in `taken` (compared without case), cut to 32 characters. */
function freeName(name: string, taken: ReadonlySet<string>): string {
  if (!taken.has(name.toLowerCase())) return name
  for (let index = 2; ; index++) {
    const suffix = `-${index}`
    const candidate = `${name.slice(0, 32 - suffix.length)}${suffix}`
    if (!taken.has(candidate.toLowerCase())) return candidate
  }
}

/** Split `npx -y pkg` written as one string into words; quotes group words. */
function splitCommandLine(text: string): string[] {
  const words: string[] = []
  for (const match of text.matchAll(/"([^"]*)"|'([^']*)'|(\S+)/gu)) words.push(match[1] ?? match[2] ?? match[3]!)
  return words
}

/**
 * The command words of a server: `command` + `args`, opencode's command array, Zed's `{ path, args, env }`, or a whole
 * command line in `command` or in `args`. `split` says a string was split into words.
 */
function commandWords(value: JsonObject): { words: string[]; env?: Json | undefined; split: boolean } | { error: true } {
  let command = value.command
  let args: Json | undefined = value.args
  let env: Json | undefined
  if (isObject(command)) {
    args = command.args ?? args
    env = command.env
    command = command.path ?? command.command ?? null
  }
  let split = false
  if (Array.isArray(command)) {
    const words = command.map(scalar)
    if (words.some(word => word === undefined)) return { error: true }
    command = words[0] ?? null
    args = [...words.slice(1) as string[], ...Array.isArray(args) ? args : []]
  }
  const program = scalar(command ?? undefined)?.trim()
  if (program === undefined || program === '') return { words: [], env, split }
  let rest: string[]
  if (args === undefined || args === null) rest = []
  else if (typeof args === 'string') { rest = splitCommandLine(args); split = true }
  else if (Array.isArray(args)) {
    const list = args.map(scalar)
    if (list.some(word => word === undefined)) return { error: true }
    rest = list as string[]
  } else return { error: true }
  // README snippets sometimes put the whole command line in `command`; a path with spaces stays whole.
  if (rest.length === 0 && /\s/u.test(program) && !/^[A-Za-z]:[\\/]|^[\\/~.]/u.test(program)) {
    const words = splitCommandLine(program)
    return { words, env, split: words.length > 1 }
  }
  return { words: [program, ...rest], env, split }
}

/** A string map (environment) with every value written out as text; `null` values are left out. */
function stringMap(value: Json | undefined): [string, string][] | undefined {
  if (value === undefined || value === null) return []
  if (!isObject(value)) return undefined
  const entries: [string, string][] = []
  for (const [key, entry] of Object.entries(value)) {
    if (entry === null) continue
    const text = scalar(entry)
    if (text === undefined) return undefined
    entries.push([key, text])
  }
  return entries
}

/** Request headers as a map, a list of `{ name, value }` (the MCP Registry) or a list of `Name: value` lines. */
function headerList(value: Json | undefined): [string, string][] | undefined {
  if (!Array.isArray(value)) return stringMap(value)
  const entries: [string, string][] = []
  for (const item of value) {
    if (typeof item === 'string') {
      const colon = item.indexOf(':')
      if (colon <= 0) return undefined
      entries.push([item.slice(0, colon).trim(), item.slice(colon + 1).trim()])
    } else if (isObject(item) && typeof item.name === 'string' && scalar(item.value) !== undefined) {
      entries.push([item.name, scalar(item.value)!])
    } else return undefined
  }
  return entries
}

/** A string list such as `includeTools`; anything else is no list. */
function names(value: Json | undefined): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string' && item !== '') : []
}

/** The first placeholder in some fields, as a note. */
function placeholderNote(fields: readonly (readonly [string, string])[]): McpImportMessage | undefined {
  for (const [field, text] of fields) {
    const placeholder = PLACEHOLDER.exec(text)?.[0]
    if (placeholder !== undefined) return { key: 'mcpJsonPlaceholder', vars: { field, value: placeholder } }
  }
  return undefined
}

/** A path written for another system than the one the Host runs commands on. */
function foreignPath(word: string, environment: McpImportEnvironment | undefined): boolean {
  if (environment === undefined) return false
  return environment === 'Windows' ? /^\/(?:Users|home)\//u.test(word) : /^[A-Za-z]:[\\/]/u.test(word)
}

/** Shells and interpreters that run code given as an argument. */
const CODE_RUNNER = /^(?:.*[\\/])?(?:bash|sh|zsh|dash|ksh|fish|powershell|pwsh|node|python[\d.]*|py|deno|bun|ruby|perl|php)(?:\.exe)?$/iu
/** Programs that pass a command line on to a shell. */
const LAUNCHER = /^(?:.*[\\/])?(?:cmd|wsl|ssh)(?:\.exe)?$/iu
/** Options that hand an interpreter code: `-c`, clustered as in `bash -lc`, `-e`, `--eval`, `-Command`… (not cmd's `/c`). */
const SCRIPT_FLAG = /^(?:-[eilx]{0,3}c|-e|--eval|-p|--print|-Command|-EncodedCommand|-ec)$/iu
/** Characters that make an argument shell code rather than a value. */
const SHELL_CODE = /[;|&<>$`'"(){}]/u

/** For each word, whether one of `pattern` came at or before it. */
function seenBy(words: readonly string[], pattern: RegExp): boolean[] {
  let seen = false
  return words.map(word => (seen ||= pattern.test(word)))
}

/**
 * Whether a command runs code written into its arguments (`bash -lc "…; curl … | sh"`, `node -e …`,
 * `powershell -NoProfile -Command …`) rather than a package or a file: the preview shows it in full and asks the user
 * to check it.
 */
function inlineScript(program: string, args: readonly string[]): boolean {
  const words = [program, ...args]
  const runner = seenBy(words, CODE_RUNNER)
  return args.some(arg => /\s/u.test(arg) && /;|\||&&|`|\$\(/u.test(arg)) || words.some((word, index) => index > 0 && runner[index - 1] === true && SCRIPT_FLAG.test(word))
}

const REMOTE_TYPES = /^(?:http|streamable[-_]?http|remote|sse)$/u
const LOCAL_TYPES = /^(?:stdio|local|command)$/u

/** Read one server; the name is assigned by the caller. */
function readServer(source: string, entry: Json, environment: McpImportEnvironment | undefined): { server: ServerFields; notes: McpImportMessage[] } | McpImportMessage {
  const name = source === '' ? '—' : source
  if (!isObject(entry)) return { key: 'mcpJsonServerInvalid', vars: { name } }
  // Continue nests the command or URL in `transport`.
  const value: JsonObject = isObject(entry.transport) ? { ...entry.transport, ...entry, transport: entry.transport.type ?? null } : entry
  const notes: McpImportMessage[] = []
  const declared = firstText(value, ['type', 'transportType', 'transport'])?.toLowerCase().replace(/\s+/gu, '')
  if (declared !== undefined && !LOCAL_TYPES.test(declared) && !REMOTE_TYPES.test(declared)) return { key: 'mcpJsonUnknownType', vars: { name, type: declared } }
  const url = firstText(value, ['url', 'serverUrl', 'httpUrl', 'baseUrl', 'endpoint'])
  const command = value.command
  const hasCommand = (typeof command === 'string' && command.trim() !== '') || (Array.isArray(command) && command.length > 0) || isObject(command)
  let enabled = value.disabled !== true && value.enabled !== false && value.isActive !== false
  // Allow-lists become Rainy's allowed tools; deny-lists need the server's tool list, which only connecting gives.
  const include = names(value.includeTools ?? value.enabled_tools ?? value.tools)
  const exclude = names(value.excludeTools ?? value.disabledTools ?? value.disabled_tools)
  const everything = include.length === 0 || include.includes('*')
  const tools = everything ? [] : [...new Set(include.filter(tool => !exclude.includes(tool)))]
  if (exclude.length > 0 && everything) notes.push({ key: 'mcpJsonExcludeTools' })
  // Rainy's empty list allows every tool; a server the original allowed none of comes in switched off instead.
  if (!everything && tools.length === 0) { enabled = false; notes.push({ key: 'mcpJsonNoTools' }) }
  if (value.oauth !== undefined || value.headersHelper !== undefined || value.authProviderType !== undefined) notes.push({ key: 'mcpJsonOAuth' })
  if (url !== undefined && (!hasCommand || (declared !== undefined && REMOTE_TYPES.test(declared)))) {
    let parsed: URL | undefined
    try { parsed = new URL(url) } catch (_invalid) { parsed = undefined }
    if (parsed === undefined || (parsed.protocol !== 'http:' && parsed.protocol !== 'https:')) return { key: 'mcpJsonBadUrl', vars: { name } }
    const options = isObject(value.requestOptions) ? value.requestOptions : isObject(value.requestInit) ? value.requestInit : undefined
    const headers = headerList(value.headers ?? value.http_headers ?? options?.headers)
    if (headers === undefined) return { key: 'mcpJsonBadHeader', vars: { name, key: 'headers' } }
    // Continue's `apiKey` is sent as a bearer token.
    const apiKey = firstText(value, ['apiKey'])
    if (apiKey !== undefined && !headers.some(([key]) => key.toLowerCase() === 'authorization')) headers.push(['Authorization', `Bearer ${apiKey}`])
    const seen = new Set<string>()
    for (const [key, header] of headers) {
      if (!HEADER_NAME.test(key) || !HEADER_VALUE.test(header) || seen.has(key.toLowerCase())) return { key: 'mcpJsonBadHeader', vars: { name, key } }
      seen.add(key.toLowerCase())
    }
    const placeholder = placeholderNote([['URL', url], ...headers])
    if (placeholder !== undefined) notes.push(placeholder)
    return {
      server: { enabled, transport: 'streamable-http', url, ...headers.length === 0 ? {} : { headers: Object.fromEntries(headers) }, tools },
      notes,
    }
  }
  const read = commandWords(value)
  if ('error' in read) return { key: 'mcpJsonBadArg', vars: { name } }
  let [program, ...args] = read.words
  if (program === undefined) return { key: 'mcpJsonNoCommand', vars: { name } }
  if (read.split) notes.push({ key: 'mcpJsonCommandSplit' })
  // `cmd /c npx …` and `npx.cmd` are how Windows clients start Node tools; a Linux Host runs the tool itself. WSL can
  // still start Windows programs named with `.exe`, so `cmd.exe` and `uvx.exe` stay there.
  if (environment !== undefined && environment !== 'Windows') {
    const shell = environment === 'SSH' ? /^cmd(?:\.exe)?$/iu : /^cmd$/iu
    const wrapped = shell.test(program) && /^\/c$/iu.test(args[0] ?? '') && args[1] !== undefined
    if (wrapped) [program, ...args] = args.slice(1) as [string, ...string[]]
    // `cmd /c "npx -y pkg"` hands cmd the whole command line as one argument.
    if (wrapped && args.length === 0 && /\s/u.test(program)) [program, ...args] = splitCommandLine(program) as [string, ...string[]]
    const suffix = environment === 'SSH' ? /^[^\\/]+\.(?:cmd|bat|exe)$/iu : /^[^\\/]+\.(?:cmd|bat)$/iu
    const bare = suffix.test(program)
    if (bare) program = program.replace(/\.(?:cmd|bat|exe)$/iu, '')
    // Only the program: its arguments may hold a token the preview hides.
    if (wrapped || bare) notes.push({ key: 'mcpJsonUnwrapped', vars: { command: program } })
  }
  // `cmd /c ""`, or a command line that starts with an empty quoted word, leaves nothing to start.
  if (program === undefined || program.trim() === '') return { key: 'mcpJsonNoCommand', vars: { name } }
  const env = stringMap(value.env ?? value.environment ?? read.env)
  if (env === undefined) return { key: 'mcpJsonBadEnv', vars: { name, key: 'env' } }
  for (const [key, text] of env) {
    if (!ENV_NAME.test(key) || /[\r\n]/u.test(text)) return { key: 'mcpJsonBadEnv', vars: { name, key } }
  }
  if (/[\r\n]/u.test(program) || args.some(arg => /[\r\n]/u.test(arg))) return { key: 'mcpJsonBadArg', vars: { name } }
  const script = inlineScript(program, args)
  if (script) notes.push({ key: 'mcpJsonInlineScript' })
  if (firstText(value, ['cwd']) !== undefined) notes.push({ key: 'mcpJsonCwd' })
  if (value.envFile !== undefined) notes.push({ key: 'mcpJsonEnvFile' })
  const elsewhere = [program, ...args].find(word => foreignPath(word, environment))
  if (elsewhere !== undefined) notes.push({ key: 'mcpJsonForeignPath', vars: { value: elsewhere } })
  // In a script `$NAME` is the script's own variable, not a value left to fill in.
  const placeholder = placeholderNote([...script ? [] : [program, ...args].map(arg => ['args', arg] as const), ...env])
  if (placeholder !== undefined) notes.push(placeholder)
  return {
    server: { enabled, transport: 'stdio', command: program, ...args.length === 0 ? {} : { args }, ...env.length === 0 ? {} : { env: Object.fromEntries(env) }, tools },
    notes,
  }
}

/** What the text is when it is not JSON: a TOML or YAML file, or a command line from a README. */
function notJson(text: string): McpImportMessage | undefined {
  // Line by line: a multiline `^\s*` would backtrack over every run of blank lines.
  const lines = text.split('\n').map(line => line.trim())
  if (lines.some(line => /^\[mcp_servers[.\]]/u.test(line))) return { key: 'mcpJsonToml' }
  if (lines.some(line => /^mcpServers\s*:$/u.test(line) || /^-\s+name\s*:/u.test(line))) return { key: 'mcpJsonYaml' }
  if (lines.some(line => /^(?:claude|codex|gemini|qwen|kimi|code)\s+(?:mcp\s+add|--add-mcp)\b/u.test(line) || line.includes('@smithery/cli'))) return { key: 'mcpJsonCommandLine' }
  return undefined
}

/** `claude mcp add-json <name> '<json>'`: the server object and its name. */
function addJsonCommand(text: string): { name: string; json: string } | undefined {
  const match = /^\s*claude\s+mcp\s+add-json\s+(?:(?:-s|--scope)\s+\S+\s+)*("[^"]+"|\S+)\s+'([\s\S]*)'(?:\s+(?:-s|--scope)\s+\S+)*\s*$/u.exec(text)
  return match === null ? undefined : { name: match[1]!.replace(/^"|"$/gu, ''), json: match[2]! }
}

/** What a server runs or reaches, without its name, switch or tool choice: two servers alike here are the same server. */
function serverIdentity(server: ServerFields): string {
  const sorted = (entries: [string, string][]): [string, string][] => entries.sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)
  return JSON.stringify([server.transport, server.command ?? '', server.args ?? [], sorted(Object.entries(server.env ?? {})), server.url ?? '',
    sorted(Object.entries(server.headers ?? {}).map(([key, value]): [string, string] => [key.toLowerCase(), value]))])
}

/**
 * Read MCP servers from pasted JSON.
 * @param text What the user pasted or the file chosen.
 * @param existing The servers already configured, or just their names: new ones get free names next to them, and one
 *   that runs the same as a configured server is left out, so pasting a configuration again adds no copies.
 * @param environment Where the Host runs commands, so commands and paths written for another system are pointed out.
 * @returns The servers to add with notes for each, the entries that could not be read, or why nothing could be read.
 */
export function parseMcpJson(text: string, existing: readonly (string | McpServerConfig)[], environment?: McpImportEnvironment): McpImportResult {
  if (text.length > MCP_JSON_MAX_LENGTH) return { ok: false, error: { key: 'mcpJsonTooLarge' } }
  const trimmed = text.trim()
  if (trimmed === '') return { ok: false, error: { key: 'mcpJsonEmpty' } }
  let entries: [string, Json][]
  let backslashes = false
  const addJson = addJsonCommand(trimmed)
  if (/^https?:\/\/\S+$/u.test(trimmed)) {
    // A bare address from a server's page.
    entries = [['', { url: trimmed }]]
  } else {
    const read = readValues(addJson?.json ?? trimmed)
    if ('error' in read) return { ok: false, error: notJson(trimmed) ?? { key: 'mcpJsonInvalid', vars: { detail: read.error } } }
    backslashes = read.backslashes
    entries = read.values.flatMap(serverEntries)
    if (addJson !== undefined && entries.length === 1) entries = [[addJson.name, entries[0]![1]]]
  }
  if (entries.length === 0) return { ok: false, error: notJson(trimmed) ?? { key: 'mcpJsonNoServers' } }
  if (entries.length > MCP_JSON_MAX_SERVERS) return { ok: false, error: { key: 'mcpJsonTooMany', vars: { max: MCP_JSON_MAX_SERVERS } } }
  const taken = new Set(existing.map(entry => (typeof entry === 'string' ? entry : entry.name).toLowerCase()))
  const configured = new Map(existing.flatMap(entry => typeof entry === 'string' ? [] : [[serverIdentity(entry), entry.name] as const]))
  const servers: ImportedServer[] = []
  const problems: McpImportMessage[] = []
  // The same server copied twice, from two tabs of a README, is read once.
  const seen = new Set<string>()
  for (const [source, value] of entries) {
    const result = readServer(source, value, environment)
    if ('key' in result) { problems.push(result); continue }
    const identity = serverIdentity(result.server)
    const same = configured.get(identity)
    if (same !== undefined) { problems.push({ key: 'mcpJsonDuplicate', vars: { name: source === '' ? '—' : source, existing: same } }); continue }
    if (seen.has(identity)) continue
    seen.add(identity)
    // A server written without a usable name is called after its package or site.
    const name = freeName(cleanServerName(source, '') || cleanServerName(unnamedServerHint(result.server), 'server'), taken)
    taken.add(name.toLowerCase())
    const notes = source !== '' && name !== source ? [{ key: 'mcpJsonRenamed' as const, vars: { from: source, to: name } }, ...result.notes] : result.notes
    servers.push({ source, server: { name, ...result.server }, notes })
  }
  return { ok: true, servers, problems, notices: backslashes ? [{ key: 'mcpJsonBackslashes' }] : [] }
}

/**
 * Whether pasted text is clearly an MCP configuration rather than a value for one field, so a paste into an empty new
 * server form imports it: a server container, a server with its command, or a named server fragment. A lone
 * `{"url": …}` may be a setting the server itself needs, so it is pasted as it is.
 */
export function looksLikeMcpJson(text: string): boolean {
  const trimmed = text.trim().replace(/^```[^\n]*\n/u, '')
  if (/^claude\s+mcp\s+add-json\s/u.test(trimmed)) return true
  if (/^[{[]/u.test(trimmed) && /"(?:mcpServers|mcp_servers|servers|context_servers)"\s*:\s*[{[]/u.test(trimmed)) return true
  if (/^\{/u.test(trimmed) && /"command"\s*:/u.test(trimmed)) return true
  return /^"[^"\n]+"\s*:\s*\{/u.test(trimmed) && /"(?:command|url|serverUrl|httpUrl|baseUrl)"\s*:/u.test(trimmed)
}

/** Names of query parameters, options and variables whose values are secrets. */
const SECRET_NAME = /key|token|secret|passw|auth|bearer|credential|sig|(?:^|[-_])pat$/iu
/** Path segments that are keys: `sk-…`, `ghp_…`, or long runs of letters and digits such as per-user endpoint IDs. */
const SECRET_SEGMENT = /^(?:sk|pk|ak|rk|ghp|gho|ghs|github_pat|xox[abp])[-_]|^(?=[A-Za-z0-9_-]*\d)(?=[A-Za-z0-9_-]*[A-Za-z])[A-Za-z0-9_-]{24,}$/u

/** Identifies a server read from the text across previews, which may rename it or move it in the list. */
export function importKey(entry: ImportedServer): string {
  return JSON.stringify([entry.source, serverIdentity(entry.server)])
}

/**
 * Text of a chosen configuration file: UTF-8, UTF-16 with its byte-order mark (Windows PowerShell's `>`), or else
 * GB18030, which covers the GBK that Chinese Windows writes.
 */
export function decodeConfigFile(bytes: Uint8Array): string {
  if (bytes[0] === 0xff && bytes[1] === 0xfe) return new TextDecoder('utf-16le').decode(bytes.subarray(2))
  if (bytes[0] === 0xfe && bytes[1] === 0xff) return new TextDecoder('utf-16be').decode(bytes.subarray(2))
  try { return new TextDecoder('utf-8', { fatal: true }).decode(bytes) } catch (_notUtf8) { return new TextDecoder('gb18030').decode(bytes) }
}

/** An address for showing on screen, with its password, key-like path segments and secret query values hidden. */
export function maskedUrl(url: string): string {
  const hashAt = url.indexOf('#')
  const beforeHash = hashAt < 0 ? url : url.slice(0, hashAt)
  const queryAt = beforeHash.indexOf('?')
  const base = queryAt < 0 ? beforeHash : beforeHash.slice(0, queryAt)
  const authorityAt = base.indexOf('//')
  const pathAt = authorityAt < 0 ? -1 : base.indexOf('/', authorityAt + 2)
  const authority = (pathAt < 0 ? base : base.slice(0, pathAt)).replace(/^([^/]*\/\/[^/@:]*:)[^/@]*@/u, '$1•••@')
  const path = pathAt < 0 ? '' : base.slice(pathAt).split('/').map(segment => SECRET_SEGMENT.test(segment) ? '•••' : segment).join('/')
  const query = queryAt < 0 ? '' : `?${beforeHash.slice(queryAt + 1).split('&').map((pair) => {
    const equals = pair.indexOf('=')
    return equals > 0 && SECRET_NAME.test(pair.slice(0, equals)) ? `${pair.slice(0, equals)}=•••` : pair
  }).join('&')}`
  return `${authority}${path}${query}${hashAt < 0 ? '' : url.slice(hashAt)}`
}

/**
 * A command line for showing on screen, with the values of secret-looking options, variables, headers and URL
 * passwords hidden. A word with spaces in it is code or a phrase the server runs, so it is always shown in full.
 */
export function maskedCommand(words: readonly string[]): string {
  const secretFlag = (word: string): boolean => /^--?[A-Za-z]/u.test(word) && !word.includes('=') && SECRET_NAME.test(word.replace(/^--?/u, ''))
  const url = (word: string): boolean => /^[a-z][a-z0-9+.-]*:\/\/\S*$/iu.test(word)
  const seen = seenBy(words, new RegExp(`${CODE_RUNNER.source}|${LAUNCHER.source}`, 'iu'))
  return words.map((word, index) => {
    const previous = index > 0 ? words[index - 1]! : ''
    // `--header "X-API-Key: …"`, mcp-remote's way of passing a token.
    if (/^(?:--header|-H)$/u.test(previous)) return word.replace(/^([^:\s]*:[ \t]*)\S+$|^([^:\s]*:[ \t]*Bearer[ \t]+)\S+$/iu, (_, plain?: string, bearer?: string) => `${plain ?? bearer}•••`)
    if (/^authorization:[ \t]*(?:bearer[ \t]+)?\S+$/iu.test(word)) return word.replace(/^(authorization:[ \t]*(?:bearer[ \t]+)?)\S+$/iu, '$1•••')
    // `postgresql://user:password@host/db`, `https://…?api_key=…`
    if (url(word)) return maskedUrl(word)
    // Once an interpreter runs, code given to `-c`, `-lc`, `-e` or `-Command`, or written with spaces or shell punctuation,
    // is shown as it is: the user has to see what will run. Without one, arguments are values and `docker -e` is not code.
    if (/\s/u.test(word) || (index > 0 && seen[index - 1] === true && (SCRIPT_FLAG.test(previous) || SHELL_CODE.test(word)))) return word
    if (secretFlag(previous)) return '•••'
    const assignment = /^(-{0,2})([^=\s]+)=(.+)$/u.exec(word)
    if (assignment !== null && url(assignment[3]!)) return `${assignment[1]}${assignment[2]}=${maskedUrl(assignment[3]!)}`
    if (assignment !== null && SECRET_NAME.test(assignment[2]!)) return `${assignment[1]}${assignment[2]}=•••`
    return word
  }).join(' ')
}
