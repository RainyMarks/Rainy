/** Read a project for its map: list the files (respecting .gitignore), count their lines and follow their imports. */
import { opendir, readFile, stat } from 'node:fs/promises'
import { join } from 'node:path'
import type { MapFile, MapPackage, ProjectMap } from '../../shared/map-protocol.ts'
import { scrubbedEnv, spawnProcess } from '../process.ts'
import { parseImports, resolveImport, type ImportContext } from './imports.ts'
import { fileType } from './languages.ts'
import { declaredDependencies, detectStack, isManifest } from './stack.ts'

/** Limits of one scan. */
export interface ScanLimits {
  /** Files listed; the rest are left out. */
  readonly maxFiles: number
  /** Files read for lines and imports. */
  readonly maxReadFiles: number
  /** Bytes read in total. */
  readonly maxReadBytes: number
  /** Larger text files are not read: their lines are estimated from their size. */
  readonly maxFileBytes: number
  /** The scan stops reading after this long and estimates the rest. */
  readonly deadlineMs: number
}

export const DEFAULT_SCAN_LIMITS: ScanLimits = {
  maxFiles: 20_000, maxReadFiles: 8_000, maxReadBytes: 96 * 1024 * 1024, maxFileBytes: 1024 * 1024, deadlineMs: 15_000,
}

/** Directories never mapped, whatever .gitignore says. */
const EXCLUDED = ['.git', 'node_modules', '.venv', 'venv', '__pycache__', '.mypy_cache', '.pytest_cache', '.ruff_cache', '.tox', '.gradle',
  '.next', '.nuxt', '.svelte-kit', '.turbo', '.cache', '.idea', '.vs']
/** Imports are looked for in files up to this size. */
const MAX_IMPORT_BYTES = 512 * 1024
/** Bytes per line when a file's lines are estimated from its size. */
const BYTES_PER_LINE = 36
const READERS = 24
const LIST_TIMEOUT_MS = 20_000
const MAX_LIST_BYTES = 64 * 1024 * 1024

/** The scanned part of a map. */
export type ScanResult = Pick<ProjectMap, 'files' | 'imports' | 'packages' | 'stack' | 'truncated'>

/** Directories of third-party code copied into a project. */
const VENDOR_DIRECTORY = /(?:^|\/)(?:vendors?|third[_-]?party|external|extern)\//iu
/** Minified or bundled output. */
const GENERATED_NAME = /\.min\.(?:js|css|mjs)$|[.-]bundle\.(?:js|mjs)$/iu
/** Average characters per line above which a large text file is minified output. */
const MINIFIED_LINE_LENGTH = 400

/**
 * Patterns `.gitattributes` marks `linguist-vendored` or `linguist-generated`, as regular expressions.
 * @param text The root `.gitattributes`.
 * @returns One expression per pattern.
 */
export function vendoredPatterns(text: string): RegExp[] {
  const patterns: RegExp[] = []
  for (const line of text.split('\n')) {
    const [pattern, ...attributes] = line.trim().split(/\s+/u)
    if (pattern === undefined || pattern === '' || pattern.startsWith('#')) continue
    if (!attributes.some(attribute => /^linguist-(?:vendored|generated)(?:=true)?$/u.test(attribute))) continue
    const glob = pattern.replace(/\/$/u, '')
    // Like .gitignore: a pattern with a slash is relative to the project, one without matches a name anywhere.
    // A directory's files count as matched too, whether or not the pattern ends in `/**`.
    const source = glob.includes('/') ? `^${globSource(glob.replace(/^\//u, ''))}(?:/.*)?$` : `(?:^|/)${globSource(glob)}(?:/.*)?$`
    try { patterns.push(new RegExp(source, 'u')) } catch (_unsupportedPattern) { /* Skip patterns this reader cannot express. */ }
  }
  return patterns
}

/** A gitattributes glob as a regular expression source: `**` spans directories, `*` and `?` stay within one. */
function globSource(glob: string): string {
  let source = ''
  for (let index = 0; index < glob.length; index++) {
    const char = glob[index]!
    if (char === '*' && glob[index + 1] === '*') {
      if (glob[index + 2] === '/') { source += '(?:.*/)?'; index += 2 } else { source += '.*'; index += 1 }
    } else if (char === '*') source += '[^/]*'
    else if (char === '?') source += '[^/]'
    else source += char.replace(/[.+^${}()|[\]\\]/u, '\\$&')
  }
  return source
}

async function listWithRipgrep(root: string, ripgrep: string, signal: AbortSignal | undefined): Promise<string[] | undefined> {
  const args = ['--files', '--hidden', '--no-require-git', '--no-messages', '--path-separator', '/', '--sort', 'path',
    ...EXCLUDED.flatMap(name => ['--glob', `!**/${name}/**`, '--glob', `!${name}`])]
  const handle = spawnProcess({ argv: [ripgrep, ...args], cwd: root, env: scrubbedEnv(), stdin: 'ignore', graceMs: 500, signal })
  const timer = setTimeout(() => { handle.terminate() }, LIST_TIMEOUT_MS)
  const chunks: Buffer[] = []
  let size = 0
  try {
    const pump = (async () => {
      for await (const chunk of handle.stdout) {
        if (size > MAX_LIST_BYTES) continue
        size += (chunk as Buffer).length
        chunks.push(chunk as Buffer)
      }
    })()
    const drain = (async () => { for await (const _chunk of handle.stderr) { /* Unreadable directories are skipped. */ } })()
    const [outcome] = await Promise.all([handle.done, pump, drain])
    // 1: nothing listed; 2: some paths could not be read, the rest is valid.
    if (outcome.exitCode !== 0 && outcome.exitCode !== 1 && outcome.exitCode !== 2) return undefined
    return Buffer.concat(chunks).toString('utf8').split('\n').map(line => line.replace(/\r$/u, '').replace(/^\.\//u, '')).filter(Boolean)
  } catch (_failedToStart) {
    return undefined
  } finally {
    clearTimeout(timer)
    handle.terminate()
    await handle.waitForExit()
  }
}

async function listByWalking(root: string, limit: number, signal: AbortSignal | undefined): Promise<string[]> {
  const excluded = new Set(EXCLUDED)
  const paths: string[] = []
  const pending = ['']
  while (pending.length > 0 && paths.length <= limit) {
    signal?.throwIfAborted()
    const directory = pending.shift()!
    let handle
    try { handle = await opendir(join(root, directory)) } catch (_unreadable) { continue }
    const children: string[] = []
    for await (const entry of handle) {
      const path = directory === '' ? entry.name : `${directory}/${entry.name}`
      if (entry.isDirectory()) { if (!excluded.has(entry.name)) children.push(path) } else if (entry.isFile()) paths.push(path)
    }
    pending.push(...children.sort())
  }
  return paths.sort()
}

function countLines(text: string): number {
  if (text.length === 0) return 0
  let lines = 0
  let index = text.indexOf('\n')
  while (index !== -1) { lines++; index = text.indexOf('\n', index + 1) }
  return text.endsWith('\n') ? lines : lines + 1
}

/** @returns The code cells of a Jupyter notebook, joined. */
function notebookCode(text: string): string {
  try {
    const notebook = JSON.parse(text) as { cells?: { cell_type?: unknown; source?: unknown }[] }
    return (notebook.cells ?? []).filter(cell => cell.cell_type === 'code')
      .map(cell => Array.isArray(cell.source) ? cell.source.join('') : typeof cell.source === 'string' ? cell.source : '').join('\n')
  } catch (_invalidNotebook) {
    return ''
  }
}

/** `compilerOptions.paths` of a tsconfig, relative to the project. */
function aliasesOf(text: string): ImportContext['aliases'] {
  try {
    // tsconfig allows comments and trailing commas.
    // `//` after a colon is part of a URL such as `$schema`, not a comment.
    const json = JSON.parse(text.replace(/\/\*[\s\S]*?\*\//gu, '').replace(/(^|[^:])\/\/.*$/gmu, '$1').replace(/,(\s*[}\]])/gu, '$1')) as {
      compilerOptions?: { baseUrl?: unknown; paths?: Record<string, unknown> }
    }
    const base = typeof json.compilerOptions?.baseUrl === 'string' ? json.compilerOptions.baseUrl.replace(/^\.\/?/u, '').replace(/\/$/u, '') : ''
    return Object.entries(json.compilerOptions?.paths ?? {}).flatMap(([pattern, targets]) => {
      if (!Array.isArray(targets)) return []
      const prefix = pattern.replace(/\*$/u, '')
      const resolved = targets.filter((target): target is string => typeof target === 'string')
        .map(target => `${base === '' ? '' : `${base}/`}${target.replace(/^\.\//u, '').replace(/\*$/u, '')}`)
      return prefix === '' ? [] : [{ prefix, targets: resolved }]
    })
  } catch (_invalidConfig) {
    return []
  }
}

/**
 * Scan a project directory.
 * @param root Absolute primary directory.
 * @param options ripgrep for listing (a directory walk otherwise), limits and cancellation.
 * @returns Files, imports between them, external packages and the stack.
 */
export async function scanProject(root: string, options: { ripgrep?: string | undefined; limits?: ScanLimits | undefined; signal?: AbortSignal | undefined } = {}): Promise<ScanResult> {
  const limits = options.limits ?? DEFAULT_SCAN_LIMITS
  const signal = options.signal
  const started = Date.now()
  const listed = (options.ripgrep === undefined ? undefined : await listWithRipgrep(root, options.ripgrep, signal)) ?? await listByWalking(root, limits.maxFiles, signal)
  let truncated = listed.length > limits.maxFiles
  const paths = listed.slice(0, limits.maxFiles)
  const attributes = paths.includes('.gitattributes') ? vendoredPatterns(await readFile(join(root, '.gitattributes'), 'utf8').catch(() => '')) : []
  // Third-party and generated code is shown, but not as the project's own code.
  const vendored = paths.map(path => VENDOR_DIRECTORY.test(path) || GENERATED_NAME.test(path) || attributes.some(pattern => pattern.test(path)))
  const types = paths.map(fileType)
  const files: MapFile[] = paths.map((path, index) => {
    const type = types[index]!
    const kind = vendored[index] && (type.kind === 'code' || type.kind === 'test') ? 'other' : type.kind
    return { path, language: type.language, kind, lines: 0, bytes: 0 }
  })
  const texts = new Map<number, string>()
  const manifests: { path: string; text: string }[] = []
  let readFiles = 0
  let readBytes = 0
  let next = 0
  const worker = async (): Promise<void> => {
    for (;;) {
      const index = next++
      if (index >= paths.length) return
      signal?.throwIfAborted()
      const path = paths[index]!
      const type = types[index]!
      const absolute = join(root, path)
      const info = await stat(absolute).catch(() => undefined)
      if (info === undefined || !info.isFile()) continue
      const bytes = info.size
      const estimate = type.binary || type.lock ? 0 : Math.ceil(bytes / BYTES_PER_LINE)
      files[index] = { ...files[index]!, bytes, lines: estimate }
      if (type.binary || type.lock) continue
      const readable = bytes <= limits.maxFileBytes && readFiles < limits.maxReadFiles && readBytes + bytes <= limits.maxReadBytes && Date.now() - started < limits.deadlineMs
      if (!readable) { if (bytes <= limits.maxFileBytes) truncated = true; continue }
      readFiles++
      readBytes += bytes
      const buffer = await readFile(absolute).catch(() => undefined)
      if (buffer === undefined) continue
      if (buffer.subarray(0, 8192).includes(0)) { files[index] = { ...files[index]!, kind: 'asset', lines: 0 }; continue }
      let text = buffer.toString('utf8')
      if (type.language === 'jupyter') text = notebookCode(text)
      const lines = countLines(text)
      const minified = bytes > 16 * 1024 && text.length / Math.max(1, lines) > MINIFIED_LINE_LENGTH
      const kind = files[index]!.kind
      files[index] = { ...files[index]!, lines, ...(minified && (kind === 'code' || kind === 'test') ? { kind: 'other' as const } : {}) }
      if ((kind === 'code' || kind === 'test') && !minified && bytes <= MAX_IMPORT_BYTES) texts.set(index, text)
      if (isManifest(path) && path.split('/').length <= 3 && manifests.length < 12 && bytes <= 256 * 1024) manifests.push({ path, text })
    }
  }
  await Promise.all(Array.from({ length: READERS }, worker))

  const lower = new Map<string, string>()
  const directories = new Map<string, string[]>()
  const classes = new Map<string, string>()
  const basenames = new Map<string, string[]>()
  for (const path of paths) {
    lower.set(path.toLowerCase(), path)
    const slash = path.lastIndexOf('/')
    const directory = slash < 0 ? '' : path.slice(0, slash)
    const members = directories.get(directory)
    if (members === undefined) directories.set(directory, [path]); else members.push(path)
    const name = path.slice(slash + 1).toLowerCase()
    const same = basenames.get(name)
    if (same === undefined) basenames.set(name, [path]); else same.push(path)
    if (/\.(?:java|kt|scala|groovy|php)$/u.test(name)) {
      const parts = path.slice(0, path.lastIndexOf('.')).toLowerCase().split('/')
      for (let start = parts.length - 1; start >= 0; start--) {
        const key = parts.slice(start).join('/')
        if (!classes.has(key)) classes.set(key, path)
      }
    }
  }
  const tsconfig = paths.find(path => path === 'tsconfig.json') ?? paths.find(path => path === 'jsconfig.json')
  const goMod = paths.includes('go.mod') ? await readFile(join(root, 'go.mod'), 'utf8').catch(() => '') : ''
  const context: ImportContext = {
    files: new Set(paths), lower, directories, classes, basenames,
    goModule: /^module\s+(\S+)/mu.exec(goMod)?.[1],
    aliases: tsconfig === undefined ? [] : aliasesOf(await readFile(join(root, tsconfig), 'utf8').catch(() => '')),
  }
  const indexOf = new Map(paths.map((path, index) => [path, index]))
  const pairs = new Set<string>()
  const imports: number[] = []
  const packages = new Map<string, Set<number>>()
  for (const [index, text] of texts) {
    const from = paths[index]!
    const language = types[index]!.language
    for (const spec of parseImports(language, text)) {
      const target = resolveImport(context, language, from, spec)
      for (const path of target.files) {
        const to = indexOf.get(path)
        if (to === undefined || to === index || pairs.has(`${index}:${to}`)) continue
        pairs.add(`${index}:${to}`)
        imports.push(index, to)
      }
      if (target.files.length === 0 && target.external !== undefined) {
        let users = packages.get(target.external)
        if (users === undefined) { users = new Set(); packages.set(target.external, users) }
        users.add(index)
      }
    }
  }
  const packageList: MapPackage[] = [...packages].map(([name, users]) => ({ name, files: [...users].sort((left, right) => left - right) }))
    .sort((left, right) => right.files.length - left.files.length || left.name.localeCompare(right.name))
  const declared = manifests.flatMap(manifest => declaredDependencies(manifest.path, manifest.text))
  return { files, imports, packages: packageList, stack: detectStack(files, packageList, declared), truncated }
}
