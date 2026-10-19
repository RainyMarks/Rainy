/** Which tool calls run, wait for the user, or are refused, by the chat's permission mode. */
import { realpath } from 'node:fs/promises'
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path'
import type { PendingApproval, PermissionMode, PermissionRules } from '../../shared/rpc.ts'

/**
 * Tools that only read; they run in every mode. `stop_command` only ends a command this chat already started, and a
 * `task` sub-agent is given read-only tools, so neither can change anything. The browser tools only reach the
 * project's own server on this machine, in a window with no sign-ins of the user's.
 */
const READ_TOOLS: ReadonlySet<string> = new Set([
  'read', 'grep', 'glob', 'web_search', 'command_output', 'stop_command', 'task',
  'browser_open', 'browser_read', 'browser_act', 'browser_screenshot',
])
const CHANGE_TOOLS: ReadonlySet<string> = new Set(['write', 'edit'])
const SHELL_TOOLS: ReadonlySet<string> = new Set(['bash', 'pwsh'])

/** Characters that chain, redirect, substitute or group commands in Bash or PowerShell. Such commands always ask. */
const COMPOUND = /[;&|<>`$(){}\r\n]/u

/** Programs whose first argument names a subcommand, so their rule keeps both words (`git status`, `pip install`). */
const SUBCOMMAND_PROGRAMS: ReadonlySet<string> = new Set([
  'git', 'npm', 'pnpm', 'yarn', 'npx', 'pip', 'pip3', 'uv', 'poetry', 'cargo', 'go', 'dotnet', 'docker', 'kubectl', 'apt', 'apt-get',
  'brew', 'conda', 'gh', 'winget', 'choco', 'scoop', 'make',
])

/** Programs that run their arguments as another command. Rules never cover them, so each use is shown to the user. */
const WRAPPERS: ReadonlySet<string> = new Set([
  'sudo', 'doas', 'su', 'env', 'xargs', 'nohup', 'timeout', 'time', 'nice', 'watch', 'exec', 'eval', 'command', 'builtin', 'source', '.',
  'wsl', 'runas', 'start', 'start-process', 'saps', 'invoke-expression', 'iex', 'invoke-command', 'icm', 'invoke-item', 'ii', 'call',
])

/** Shells and interpreters that run inline code after flags like `-c`; such a call always asks even when the program is allowed. */
const INTERPRETERS: ReadonlySet<string> = new Set([
  'bash', 'sh', 'zsh', 'dash', 'ksh', 'fish', 'pwsh', 'powershell', 'cmd', 'python', 'python3', 'py', 'node', 'deno', 'bun', 'perl', 'ruby',
  'php', 'lua', 'osascript', 'java', 'jshell',
])
const INLINE_FLAG = /^(?:-[a-z]*[cepr][a-z]*(?:[=:'"].*)?|--(?:eval|print|command|encodedcommand)(?:=.*)?|\/[ck])$/iu

/** Model-facing reason for calls refused in read-only mode. */
export const READ_ONLY_REASON = 'This chat is in read-only mode: files cannot be changed, commands cannot run and tools that change state are '
  + 'unavailable. Describe the change or command you would make, or ask the user to switch the chat out of read-only mode.'

/** Program name as rules compare it: no directory, no `.exe`, lowercase. */
function programName(word: string): string {
  const name = word.replace(/^.*[\\/]/u, '').toLowerCase()
  return name.endsWith('.exe') ? name.slice(0, -4) : name
}

/** Words of a command that neither chains, redirects nor substitutes, or `undefined` when it does. */
function simpleWords(command: string): string[] | undefined {
  const text = command.trim()
  if (text === '' || COMPOUND.test(text)) return undefined
  const words = text.split(/\s+/u)
  const program = programName(words[0]!)
  if (WRAPPERS.has(program) || /^\w+=/u.test(words[0]!)) return undefined
  if (INTERPRETERS.has(program) && words.slice(1).some(word => INLINE_FLAG.test(word))) return undefined
  return words
}

/**
 * The rule "always allow" offers for a command.
 * @param command Command text.
 * @returns `program`, or `program subcommand` for tools like git and pip; `undefined` when the command chains, redirects,
 *   substitutes, wraps another command or runs inline code, which can only be allowed once.
 */
export function commandRule(command: string): string | undefined {
  const words = simpleWords(command)
  if (words === undefined) return undefined
  const [program, sub] = words as [string, string | undefined]
  return sub !== undefined && SUBCOMMAND_PROGRAMS.has(programName(program)) && /^[A-Za-z][\w:.-]*$/u.test(sub) ? `${program} ${sub}` : program
}

/**
 * Whether remembered rules cover a command: it must be simple and start with every word of one rule.
 * @param command Command text.
 * @param rules Remembered prefixes.
 * @param windows Compare words case-insensitively, as PowerShell and Windows file names do.
 * @returns Whether the command may run without asking.
 */
export function commandAllowed(command: string, rules: readonly string[], windows: boolean): boolean {
  const words = simpleWords(command)
  if (words === undefined) return false
  // The program is compared as written, so a rule for `./solve` does not cover a `solve` elsewhere.
  const word = (value: string, first: boolean): string => !windows ? value : first ? value.toLowerCase().replace(/\.exe$/u, '') : value.toLowerCase()
  return rules.some((rule) => {
    const parts = rule.trim().split(/\s+/u)
    return parts.length <= words.length && parts.every((part, index) => word(part, index === 0) === word(words[index]!, index === 0))
  })
}

/**
 * Resolve symbolic links in the longest existing prefix of a path, so a link inside the project cannot hide a target outside it.
 * @param path Absolute path, which need not exist.
 * @returns The canonical path.
 */
export async function canonicalPath(path: string): Promise<string> {
  const rest: string[] = []
  let current = path
  for (;;) {
    try {
      return join(await realpath(current), ...rest)
    } catch (error) {
      const code = (error as NodeJS.ErrnoException).code
      if (code !== 'ENOENT' && code !== 'ENOTDIR') return path
      const parent = dirname(current)
      if (parent === current) return path
      rest.unshift(basename(current))
      current = parent
    }
  }
}

/**
 * @param root Canonical directory.
 * @param path Canonical path.
 * @returns Whether `path` is `root` or inside it.
 */
export function within(root: string, path: string): boolean {
  const inside = relative(root, path)
  return inside === '' || (inside !== '..' && !inside.startsWith(`..${sep}`) && !isAbsolute(inside))
}

/** What the chat knows when a tool call is about to run. */
export interface PermissionInput {
  mode: PermissionMode
  toolName: string
  args: unknown
  /** Working directory that relative paths resolve against. */
  cwd: string
  /** Canonical project directories: the working directory and the project's other roots; read only for writes in ask mode. */
  roots: () => Promise<readonly string[]>
  rules: PermissionRules
  /** MCP tools their server marks as read-only. */
  readOnlyTools: ReadonlySet<string>
  /** Approvals can be remembered: the chat belongs to a project. */
  canRemember: boolean
  windows: boolean
}

/** Run, refuse with a reason for the model, or wait for the user. */
export type PermissionDecision =
  | { kind: 'allow' }
  | { kind: 'deny'; reason: string }
  | { kind: 'ask'; approval: Pick<PendingApproval, 'reason' | 'rule' | 'path'> }

const ALLOW: PermissionDecision = { kind: 'allow' }

/** @param url URL from the model. @returns Its lower-case host, or `undefined` when the tool will refuse it anyway. */
function webHost(url: string | undefined): string | undefined {
  try {
    const parsed = new URL(url?.trim() ?? '')
    return parsed.protocol === 'http:' || parsed.protocol === 'https:' ? parsed.hostname.toLowerCase() : undefined
  } catch { return undefined }
}

function stringArg(args: unknown, name: string): string | undefined {
  const value = args !== null && typeof args === 'object' ? (args as Record<string, unknown>)[name] : undefined
  return typeof value === 'string' ? value : undefined
}

/**
 * Decide one tool call.
 * @param input Mode, call and project facts.
 * @returns The decision.
 */
export async function decide(input: PermissionInput): Promise<PermissionDecision> {
  const { mode, toolName } = input
  if (READ_TOOLS.has(toolName) || mode === 'auto') return ALLOW
  // A fetched URL reaches a third party and could carry project data out, so each new site asks first, in every mode.
  if (toolName === 'web_fetch') {
    const host = webHost(stringArg(input.args, 'url'))
    if (host === undefined) return ALLOW
    const rule = `web_fetch:${host}`
    return input.rules.tools.includes(rule) ? ALLOW
      : { kind: 'ask', approval: { reason: 'web', path: stringArg(input.args, 'url'), ...(input.canRemember ? { rule } : {}) } }
  }
  const readOnlyTool = input.readOnlyTools.has(toolName)
  if (mode === 'read-only') return readOnlyTool ? ALLOW : { kind: 'deny', reason: READ_ONLY_REASON }
  if (CHANGE_TOOLS.has(toolName)) {
    const requested = stringArg(input.args, 'file_path')
    if (requested === undefined || requested.trim() === '') return ALLOW
    const target = await canonicalPath(resolve(input.cwd, requested))
    return (await input.roots()).some(root => within(root, target)) ? ALLOW : { kind: 'ask', approval: { reason: 'outside-write', path: target } }
  }
  if (SHELL_TOOLS.has(toolName)) {
    const command = stringArg(input.args, 'command') ?? ''
    if (commandAllowed(command, input.rules.commands, input.windows)) return ALLOW
    const rule = input.canRemember ? commandRule(command) : undefined
    return { kind: 'ask', approval: { reason: 'command', ...(rule === undefined ? {} : { rule }) } }
  }
  if (readOnlyTool || input.rules.tools.includes(toolName)) return ALLOW
  return { kind: 'ask', approval: { reason: 'mcp', ...(input.canRemember ? { rule: toolName } : {}) } }
}

/**
 * Model-facing text of a refused call.
 * @param reason What the user typed, if anything.
 * @returns The tool result text.
 */
export function denialText(reason: string | undefined): string {
  const said = reason?.trim() ?? ''
  return said === ''
    ? 'The user denied this tool call. Do not retry it or work around it; stop and wait for the user\'s instructions.'
    : `The user denied this tool call and said: ${said}`
}
