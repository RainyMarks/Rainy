/** Text search across a project's roots with ripgrep, and replacement in the files it found. */
import { lstat, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import type { IdeRootId, IdeWorkspaceRoot, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { localized } from '../../shared/locale.ts'
import {
  globList, previewLine, replaceInText, type TextReplaceRequest, type TextReplaceResult, type TextSearchFile, type TextSearchMatch,
  type TextSearchOptions, type TextSearchRequest, type TextSearchResult,
} from '../../shared/text-search.ts'
import { ideContains, ideRelativePath } from '../ide/files-core.ts'
import { scrubbedEnv, spawnProcess } from '../process.ts'
import { roots, type Projects } from '../projects.ts'
import { RpcError } from '../rpc.ts'
import { realpath } from '../realpath.ts'

/** Matches a search returns at most; the rest of the project is not searched. */
export const MAX_MATCHES = 5000
const SEARCH_TIMEOUT_MS = 20_000
/** Output read from ripgrep at most, against files with huge matching lines. */
const MAX_OUTPUT_BYTES = 64 * 1024 * 1024
/** Files larger than this are neither searched nor replaced in. */
const MAX_FILE_BYTES = 8 * 1024 * 1024
/** Folders left out of every search, as in the file tree's quick open. */
const EXCLUDED = ['.git', 'node_modules', '.venv', '__pycache__']

/** What the search service needs. */
export interface SearchDependencies {
  readonly projects: Pick<Projects, 'get'>
  readonly ripgrep?: string | undefined
}

interface RipgrepText { readonly text?: string; readonly bytes?: string }
interface RipgrepMatch {
  readonly type: string
  readonly data: {
    readonly path?: RipgrepText
    readonly lines?: RipgrepText
    readonly line_number?: number
    readonly submatches?: readonly { readonly start: number; readonly end: number }[]
  }
}

function decoded(value: RipgrepText | undefined): Buffer {
  if (value?.text !== undefined) return Buffer.from(value.text, 'utf8')
  return Buffer.from(value?.bytes ?? '', 'base64')
}

/**
 * Turn the typed include and exclude lists into ripgrep globs. A plain name also covers a folder of that name.
 * @param options Search options.
 * @returns `--glob` arguments.
 */
export function globArguments(options: Pick<TextSearchOptions, 'include' | 'exclude'>): string[] {
  const expand = (glob: string): string[] => {
    const clean = glob.replace(/^\.\//u, '').replace(/\\/gu, '/')
    if (clean.endsWith('/')) return [`${clean}**`]
    return /[*?[\]{}]/u.test(clean) ? [clean] : [clean, `${clean}/**`, `**/${clean}/**`]
  }
  return [
    ...globList(options.include).flatMap(expand).flatMap(glob => ['--glob', glob]),
    ...globList(options.exclude).flatMap(expand).flatMap(glob => ['--glob', `!${glob}`]),
    ...EXCLUDED.flatMap(name => ['--glob', `!**/${name}/**`, '--glob', `!${name}`]),
  ]
}

/** Search and replace over projects. */
export class TextSearch {
  /** The running search of each project; a newer one ends it. */
  private readonly running = new Map<WorkspaceId, AbortController>()

  constructor(private readonly deps: SearchDependencies) {}

  /** End every running search, for shutdown. */
  close(): void {
    for (const controller of this.running.values()) controller.abort()
    this.running.clear()
  }

  /**
   * Find every match of a pattern in a project's roots.
   * @param request Pattern, flags and globs.
   * @param signal Cancels the search.
   * @returns Matches grouped by file; at most {@link MAX_MATCHES}.
   */
  async search(request: TextSearchRequest, signal?: AbortSignal): Promise<TextSearchResult> {
    if (request.pattern === '') return { files: [], matches: 0, truncated: false }
    const ripgrep = this.deps.ripgrep
    if (ripgrep === undefined) throw new RpcError('search-unavailable', localized({ zh: '找不到 ripgrep，无法搜索。', en: 'ripgrep was not found, so files cannot be searched.' }))
    const mounted = this.roots(request.workspaceId)
    this.running.get(request.workspaceId)?.abort()
    const controller = new AbortController()
    this.running.set(request.workspaceId, controller)
    const lifetime = signal === undefined ? controller.signal : AbortSignal.any([signal, controller.signal])
    const files: TextSearchFile[] = []
    let matches = 0
    let truncated = false
    try {
      for (const root of mounted) {
        const found = await this.searchRoot(ripgrep, root, request, MAX_MATCHES - matches, lifetime)
        files.push(...found.files)
        matches += found.matches
        if (found.truncated) { truncated = true; break }
      }
    } finally {
      if (this.running.get(request.workspaceId) === controller) this.running.delete(request.workspaceId)
    }
    return { files, matches, truncated }
  }

  /**
   * Replace every match in the listed files. Files that are not UTF-8 text, too large or outside the project are reported, not changed.
   * @param request Pattern, flags, replacement and files.
   * @returns What changed in each file.
   */
  async replace(request: TextReplaceRequest): Promise<TextReplaceResult> {
    const mounted = new Map(this.roots(request.workspaceId).map(root => [root.rootId, root]))
    const done: { rootId: IdeRootId; path: string; replaced: number }[] = []
    const failed: { rootId: IdeRootId; path: string; message: string }[] = []
    for (const file of request.files) {
      const root = mounted.get(file.rootId)
      try {
        if (root === undefined) throw new Error(localized({ zh: '这个文件夹已不在项目中。', en: 'This folder is no longer in the project.' }))
        const replaced = await this.replaceFile(root, ideRelativePath(file.path), request)
        done.push({ rootId: file.rootId, path: file.path, replaced })
      } catch (error) {
        failed.push({ rootId: file.rootId, path: file.path, message: error instanceof Error ? error.message : String(error) })
      }
    }
    return { files: done, failed }
  }

  private roots(workspaceId: WorkspaceId): IdeWorkspaceRoot[] {
    const project = this.deps.projects.get(workspaceId)
    if (project === undefined) throw new RpcError('workspace-not-found', localized({ zh: '工作区不存在。', en: 'This workspace does not exist.' }))
    return roots(project)
  }

  private async replaceFile(root: IdeWorkspaceRoot, path: string, request: TextReplaceRequest): Promise<number> {
    const canonicalRoot = await realpath(root.path)
    const absolute = await realpath(join(canonicalRoot, path))
    if (!ideContains(canonicalRoot, absolute)) throw new Error(localized({ zh: '文件在项目之外。', en: 'The file is outside the project.' }))
    const info = await lstat(absolute)
    if (!info.isFile()) throw new Error(localized({ zh: '不是文件。', en: 'Not a file.' }))
    if (info.size > MAX_FILE_BYTES) throw new Error(localized({ zh: '文件太大，没有替换。', en: 'The file is too large to replace in.' }))
    const bytes = await readFile(absolute)
    const bom = bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf
    let text: string
    try { text = new TextDecoder('utf-8', { fatal: true }).decode(bom ? bytes.subarray(3) : bytes) } catch (_notUtf8) {
      throw new Error(localized({ zh: '不是 UTF-8 文本，没有替换。', en: 'Not UTF-8 text, so nothing was replaced.' }))
    }
    let result: { text: string; count: number }
    try { result = replaceInText(text, request, request.replacement) } catch (_invalid) {
      throw new Error(localized({ zh: '这个正则表达式无法用于替换。', en: 'This regular expression cannot be used to replace.' }))
    }
    if (result.count > 0) await writeFile(absolute, bom ? Buffer.concat([bytes.subarray(0, 3), Buffer.from(result.text, 'utf8')]) : result.text)
    return result.count
  }

  private async searchRoot(ripgrep: string, root: IdeWorkspaceRoot, request: TextSearchRequest, budget: number, signal: AbortSignal | undefined): Promise<TextSearchResult> {
    const args = [
      '--json', '--hidden', '--no-require-git', '--max-filesize', `${MAX_FILE_BYTES}`,
      request.caseSensitive ? '--case-sensitive' : '--ignore-case',
      ...request.regex ? ['--engine', 'auto'] : ['--fixed-strings'],
      ...request.wholeWord ? ['--word-regexp'] : [],
      ...globArguments(request),
      '--regexp', request.pattern,
    ]
    const lifetime = AbortSignal.any([...signal === undefined ? [] : [signal], AbortSignal.timeout(SEARCH_TIMEOUT_MS)])
    const handle = spawnProcess({ argv: [ripgrep, ...args], cwd: root.path, env: scrubbedEnv(), stdin: 'ignore', graceMs: 500, signal: lifetime })
    const byPath = new Map<string, TextSearchMatch[]>()
    let matches = 0
    let truncated = false
    let size = 0
    let errors = ''
    const read = (line: string): void => {
      if (line === '' || truncated) return
      let message: RipgrepMatch
      try { message = JSON.parse(line) as RipgrepMatch } catch (_partial) { return }
      if (message.type !== 'match' || message.data.line_number === undefined) return
      const path = decoded(message.data.path).toString('utf8').replaceAll('\\', '/').replace(/^\.\//u, '')
      const bytes = decoded(message.data.lines)
      const text = bytes.toString('utf8').replace(/\r?\n$/u, '')
      const found = byPath.get(path) ?? []
      for (const submatch of message.data.submatches ?? []) {
        if (matches >= budget) { truncated = true; handle.terminate(); return }
        const column = bytes.subarray(0, submatch.start).toString('utf8').length
        const length = bytes.subarray(submatch.start, submatch.end).toString('utf8').length
        found.push({ line: message.data.line_number, column, length, ...previewLine(text, column) })
        matches++
      }
      byPath.set(path, found)
    }
    try {
      const pump = (async () => {
        let rest = ''
        for await (const chunk of handle.stdout) {
          size += (chunk as Buffer).length
          if (size > MAX_OUTPUT_BYTES) { truncated = true; handle.terminate(); break }
          const lines = (rest + (chunk as Buffer).toString('utf8')).split('\n')
          rest = lines.pop() ?? ''
          for (const line of lines) read(line)
        }
        read(rest)
      })()
      const drain = (async () => { for await (const chunk of handle.stderr) if (errors.length < 4000) errors += (chunk as Buffer).toString('utf8') })()
      const [outcome] = await Promise.all([handle.done, pump, drain])
      signal?.throwIfAborted()
      if (!truncated && lifetime.aborted) truncated = true
      // 1: no match; 2: some files could not be read, or the pattern is invalid when nothing was found.
      if (outcome.exitCode === 2 && matches === 0 && /regex|parse|syntax/iu.test(errors)) {
        throw new RpcError('invalid-pattern', localized({ zh: `正则表达式有误：${errors.trim().split('\n').slice(-3).join(' ')}`, en: `Invalid regular expression: ${errors.trim().split('\n').slice(-3).join(' ')}` }))
      }
    } finally {
      handle.terminate()
      await handle.waitForExit()
    }
    const files = [...byPath].filter(([, found]) => found.length > 0).sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)
      .map(([path, found]): TextSearchFile => ({ rootId: root.rootId, path, matches: found }))
    return { files, matches, truncated }
  }
}
