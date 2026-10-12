/** `grep` and `glob`: content and file-name search through ripgrep. Both only read, so they never wait for approval. */
import { stat } from 'node:fs/promises'
import { isAbsolute, relative, resolve, sep } from 'node:path'
import { StringEnum, Type, type Static } from '@earendil-works/pi-ai'
import type { AgentTool } from '@earendil-works/pi-agent-core'
import { scrubbedEnv, spawnProcess } from '../../process.ts'
import { limitOutput, textResult, type ToolContext } from './common.ts'

/** ripgrep output read before formatting; the search stops beyond it. */
const MAX_OUTPUT_BYTES = 8 * 1024 * 1024
const SEARCH_TIMEOUT_MS = 30_000
/** Lines or files returned when the model sets no limit. */
export const DEFAULT_RESULT_LIMIT = 200
const MAX_RESULT_LIMIT = 2000
/** Files whose modification times are compared to sort glob results; beyond this, ripgrep's order is kept. */
const GLOB_SORT_LIMIT = 5000

/** Results for the UI card, in the order the model received them. */
export interface SearchDetails {
  pattern: string
  path: string
  /** More results exist than were returned. */
  truncated: boolean
  /** Files found by glob or listed by grep. */
  paths?: string[] | undefined
  /** Lines grep returned, grouped by file; context lines included. */
  files?: { path: string; matches: { lineNumber: number; line: string }[] }[] | undefined
  /** Matches per file from grep's count mode. */
  counts?: { path: string; count: number }[] | undefined
}

interface RipgrepOutcome { stdout: string; stderr: string; exitCode: number | null; truncated: boolean }

async function ripgrep(context: ToolContext, args: string[], cwd: string, signal: AbortSignal | undefined): Promise<RipgrepOutcome> {
  if (context.ripgrep === undefined) throw new Error('ripgrep is not available in this installation; use the shell tool instead')
  const handle = spawnProcess({ argv: [context.ripgrep, ...args], cwd, env: scrubbedEnv(), stdin: 'ignore', graceMs: 1000, signal })
  const timer = setTimeout(() => { handle.terminate() }, SEARCH_TIMEOUT_MS)
  const out: Buffer[] = []
  const err: Buffer[] = []
  let size = 0
  let truncated = false
  try {
    const pumpOut = (async () => {
      for await (const chunk of handle.stdout) {
        if (truncated) continue
        size += (chunk as Buffer).length
        out.push(chunk as Buffer)
        if (size > MAX_OUTPUT_BYTES) { truncated = true; handle.terminate() }
      }
    })()
    const pumpErr = (async () => { for await (const chunk of handle.stderr) if (err.length < 64) err.push(chunk as Buffer) })()
    const [outcome] = await Promise.all([handle.done, pumpOut, pumpErr])
    if (signal?.aborted) throw new Error('tool call aborted')
    let stdout = Buffer.concat(out).toString('utf8')
    // A cut stream may end inside an entry; drop the partial entry.
    if (truncated) stdout = stdout.slice(0, Math.max(0, stdout.lastIndexOf('\n'), stdout.lastIndexOf('\0')))
    return { stdout, stderr: Buffer.concat(err).toString('utf8').trim(), exitCode: outcome.exitCode, truncated }
  } finally {
    clearTimeout(timer)
    handle.terminate()
    await handle.waitForExit()
  }
}

/** Search root and how paths are shown: relative to the working directory when inside it. */
function searchRoot(context: ToolContext, requested: string | undefined): { absolute: string; display: (path: string) => string } {
  if (requested?.includes('\0') === true) throw new Error('invalid path')
  const absolute = resolve(context.cwd, requested === undefined || requested.trim() === '' ? '.' : requested)
  const display = (path: string): string => {
    const full = resolve(absolute, path)
    const inside = relative(context.cwd, full)
    return inside !== '' && inside !== '..' && !inside.startsWith(`..${sep}`) && !isAbsolute(inside) ? inside.split(sep).join('/') : full
  }
  return { absolute, display }
}

function clampLimit(limit: number | undefined): number {
  return Math.min(MAX_RESULT_LIMIT, Math.max(1, Math.floor(limit ?? DEFAULT_RESULT_LIMIT)))
}

const grepParameters = Type.Object({
  pattern: Type.String({ description: 'Regular expression in ripgrep (Rust) syntax. Escape literal characters such as { } ( ) . and |.' }),
  path: Type.Optional(Type.String({ description: 'File or directory to search. Defaults to the session workspace.' })),
  glob: Type.Optional(Type.String({ description: 'Only search files whose path matches this glob, such as "*.py" or "src/**/*.ts".' })),
  output_mode: Type.Optional(StringEnum(['files_with_matches', 'content', 'count'] as const, {
    description: '"files_with_matches" (default) lists matching files; "content" shows matching lines with line numbers; "count" shows matches per file.',
  })),
  ignore_case: Type.Optional(Type.Boolean({ description: 'Match case-insensitively.' })),
  context: Type.Optional(Type.Number({ description: 'Lines shown before and after each match in content mode.' })),
  limit: Type.Optional(Type.Number({ description: `Maximum lines or files returned. Defaults to ${DEFAULT_RESULT_LIMIT}.` })),
})

/**
 * The `grep` tool.
 * @param context Chat tool context.
 * @returns Tool definition.
 */
export function grepTool(context: ToolContext): AgentTool<typeof grepParameters, SearchDetails> {
  return {
    name: 'grep',
    label: 'Grep',
    description: 'Search file contents with a regular expression (ripgrep). Respects .gitignore, includes hidden files except .git, '
      + 'and skips binary files; use the shell for binaries.',
    parameters: grepParameters,
    executionMode: 'sequential',
    async execute(_id, params: Static<typeof grepParameters>, signal) {
      if (params.pattern === '') throw new Error('invalid pattern: expected a non-empty regular expression')
      const root = searchRoot(context, params.path)
      const mode = params.output_mode ?? 'files_with_matches'
      const limit = clampLimit(params.limit)
      // `--null` ends every printed path with NUL, so names containing ":" or "-" parse reliably.
      const args = ['--no-config', '--color', 'never', '--hidden', '--glob', '!.git', '--max-columns', '500', '--max-columns-preview', '--null']
      if (params.ignore_case === true) args.push('--ignore-case')
      if (params.glob !== undefined && params.glob.trim() !== '') args.push('--glob', params.glob)
      if (mode === 'files_with_matches') args.push('--files-with-matches')
      else if (mode === 'count') args.push('--count-matches', '--with-filename')
      else {
        args.push('--line-number', '--with-filename')
        const around = Math.min(20, Math.max(0, Math.floor(params.context ?? 0)))
        if (around > 0) args.push('--context', String(around))
      }
      const target = await stat(root.absolute).catch(() => undefined)
      if (target === undefined) throw new Error(`path not found: ${params.path ?? '.'}`)
      const cwd = target.isDirectory() ? root.absolute : resolve(root.absolute, '..')
      args.push('--regexp', params.pattern, '--', target.isDirectory() ? '.' : root.absolute)
      const outcome = await ripgrep(context, args, cwd, signal)
      if (outcome.exitCode === 2 && outcome.stdout === '') throw new Error(outcome.stderr || 'ripgrep failed')
      // ripgrep prints paths relative to its working directory; show them relative to the session workspace.
      const shownPath = (path: string): string => root.display(relative(root.absolute, resolve(cwd, path)) || '.')
      const base = { pattern: params.pattern, path: root.display('.') }
      let lines: string[]
      let details: SearchDetails
      if (mode === 'files_with_matches') {
        const paths = outcome.stdout.split('\0').filter(path => path !== '').map(shownPath)
        lines = paths.slice(0, limit)
        details = { ...base, truncated: outcome.truncated || paths.length > limit, paths: lines }
      } else if (mode === 'count') {
        const counts = outcome.stdout.split('\n').flatMap((line) => {
          const end = line.indexOf('\0')
          return end < 0 ? [] : [{ path: shownPath(line.slice(0, end)), count: Number(line.slice(end + 1)) || 0 }]
        })
        const kept = counts.slice(0, limit)
        lines = kept.map(item => `${item.path}:${item.count}`)
        details = { ...base, truncated: outcome.truncated || counts.length > limit, counts: kept }
      } else {
        const rows = outcome.stdout.split('\n').filter(line => line !== '')
        const kept = rows.slice(0, limit)
        const files: NonNullable<SearchDetails['files']> = []
        lines = kept.map((line) => {
          const end = line.indexOf('\0')
          const found = end < 0 ? null : /^(\d+)([:-])(.*)$/su.exec(line.slice(end + 1))
          if (found === null) return line
          const path = shownPath(line.slice(0, end))
          const lineNumber = Number(found[1])
          if (files.at(-1)?.path !== path) files.push({ path, matches: [] })
          files.at(-1)!.matches.push({ lineNumber, line: found[3]! })
          // Matches read path:line:text, context lines path-line-text, as grep prints them.
          return found[2] === ':' ? `${path}:${lineNumber}:${found[3]}` : `${path}-${lineNumber}-${found[3]}`
        })
        details = { ...base, truncated: outcome.truncated || rows.length > limit, files }
      }
      if (lines.length === 0) return textResult('No matches found.', details)
      const note = details.truncated ? `\n[Showing the first ${lines.length} ${mode === 'content' ? 'lines' : 'files'}; narrow the search with path or glob, or raise limit.]` : ''
      return textResult(await limitOutput(context, 'grep', lines.join('\n') + note), details)
    },
  }
}

const globParameters = Type.Object({
  pattern: Type.String({ description: 'Glob such as "**/*.py", "*.pcap" or "src/**/test_*.c". A pattern without "/" matches file names at any depth.' }),
  path: Type.Optional(Type.String({ description: 'Directory to search. Defaults to the session workspace.' })),
  limit: Type.Optional(Type.Number({ description: `Maximum files returned. Defaults to ${DEFAULT_RESULT_LIMIT}.` })),
})

/**
 * The `glob` tool.
 * @param context Chat tool context.
 * @returns Tool definition.
 */
export function globTool(context: ToolContext): AgentTool<typeof globParameters, SearchDetails> {
  return {
    name: 'glob',
    label: 'Glob',
    description: 'Find files by name pattern, most recently modified first. Respects .gitignore and includes hidden files except .git.',
    parameters: globParameters,
    executionMode: 'sequential',
    async execute(_id, params: Static<typeof globParameters>, signal) {
      if (params.pattern.trim() === '') throw new Error('invalid pattern: expected a non-empty glob')
      const root = searchRoot(context, params.path)
      const target = await stat(root.absolute).catch(() => undefined)
      if (target?.isDirectory() !== true) throw new Error(`not a directory: ${params.path ?? '.'}`)
      const limit = clampLimit(params.limit)
      const outcome = await ripgrep(context, ['--no-config', '--files', '--null', '--hidden', '--glob', '!.git', '--glob', params.pattern], root.absolute, signal)
      if (outcome.exitCode === 2 && outcome.stdout === '') throw new Error(outcome.stderr || 'ripgrep failed')
      const files = outcome.stdout.split('\0').filter(file => file !== '')
      const timed = await Promise.all(files.slice(0, GLOB_SORT_LIMIT).map(async file => ({
        file, time: (await stat(resolve(root.absolute, file)).catch(() => undefined))?.mtimeMs ?? 0,
      })))
      timed.sort((left, right) => right.time - left.time)
      const ordered = [...timed.map(item => item.file), ...files.slice(GLOB_SORT_LIMIT)]
      const shown = ordered.slice(0, limit).map(file => root.display(file))
      const truncated = outcome.truncated || ordered.length > limit
      const details: SearchDetails = { pattern: params.pattern, path: root.display('.'), truncated, paths: shown }
      if (shown.length === 0) return textResult('No files found.', details)
      const note = truncated ? `\n[Showing ${shown.length} of ${outcome.truncated ? 'many' : ordered.length} files; narrow the pattern or path.]` : ''
      return textResult(await limitOutput(context, 'glob', shown.join('\n') + note), details)
    },
  }
}
