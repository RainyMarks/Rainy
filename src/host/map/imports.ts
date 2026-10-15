/**
 * Imports of source files: what each language names, and which project files or external packages those names lead to.
 * Parsing is by pattern, not by grammar; a missed or extra import only moves a line on the map.
 */

/** One import as written. */
export interface ImportSpec {
  readonly spec: string
  /**
   * How to resolve it: `path` is relative to the importing file (or the project when it starts with `/`); `module` is a
   * language module name; `mod` and `use` are Rust declarations; `include` is a C/C++ local include.
   */
  readonly style: 'path' | 'module' | 'from' | 'mod' | 'use' | 'include' | 'class' | 'lib'
  /** Names after `from x import`, which may be submodules. */
  readonly names?: readonly string[] | undefined
}

/** What the resolver knows about the project. */
export interface ImportContext {
  /** Every project file, by exact path. */
  readonly files: ReadonlySet<string>
  /** Lower-case path → path, for case-insensitive file systems and package names. */
  readonly lower: ReadonlyMap<string, string>
  /** Directory → its files, sorted. */
  readonly directories: ReadonlyMap<string, readonly string[]>
  /** Lower-case slashed suffixes of Java, Kotlin, Scala, Groovy and PHP files without extension → path. */
  readonly classes: ReadonlyMap<string, string>
  /** Lower-case file name → paths, for C and C++ includes that name a file by a partial path. */
  readonly basenames: ReadonlyMap<string, readonly string[]>
  /** `module` line of the root `go.mod`. */
  readonly goModule?: string | undefined
  /** `compilerOptions.paths` of the root `tsconfig.json` or `jsconfig.json`: alias prefix → target prefixes. */
  readonly aliases: readonly { readonly prefix: string; readonly targets: readonly string[] }[]
}

/** Where an import leads. */
export interface ImportTarget {
  /** Project files. */
  readonly files: readonly string[]
  /** An external package, when no project file matched. */
  readonly external?: string | undefined
}

const JS_EXTENSIONS = ['.ts', '.tsx', '.js', '.jsx', '.mjs', '.cjs', '.mts', '.cts', '.vue', '.svelte', '.json', '.d.ts']
const NODE_BUILTINS = new Set(['assert', 'async_hooks', 'buffer', 'child_process', 'cluster', 'console', 'constants', 'crypto', 'dgram',
  'diagnostics_channel', 'dns', 'domain', 'events', 'fs', 'http', 'http2', 'https', 'inspector', 'module', 'net', 'os', 'path', 'perf_hooks',
  'process', 'punycode', 'querystring', 'readline', 'repl', 'stream', 'string_decoder', 'sys', 'timers', 'tls', 'trace_events', 'tty', 'url',
  'util', 'v8', 'vm', 'wasi', 'worker_threads', 'zlib', 'test', 'sqlite'])
const PYTHON_STDLIB = new Set(['__future__', 'abc', 'argparse', 'array', 'ast', 'asyncio', 'atexit', 'base64', 'binascii', 'bisect', 'builtins',
  'bz2', 'calendar', 'cmath', 'cmd', 'codecs', 'collections', 'colorsys', 'concurrent', 'configparser', 'contextlib', 'contextvars', 'copy',
  'copyreg', 'cProfile', 'csv', 'ctypes', 'curses', 'dataclasses', 'datetime', 'dbm', 'decimal', 'difflib', 'dis', 'doctest', 'email',
  'encodings', 'enum', 'errno', 'faulthandler', 'fcntl', 'filecmp', 'fileinput', 'fnmatch', 'fractions', 'ftplib', 'functools', 'gc',
  'getopt', 'getpass', 'gettext', 'glob', 'graphlib', 'grp', 'gzip', 'hashlib', 'heapq', 'hmac', 'html', 'http', 'imaplib', 'importlib',
  'inspect', 'io', 'ipaddress', 'itertools', 'json', 'keyword', 'linecache', 'locale', 'logging', 'lzma', 'mailbox', 'marshal', 'math',
  'mimetypes', 'mmap', 'msvcrt', 'multiprocessing', 'netrc', 'numbers', 'operator', 'optparse', 'os', 'pathlib', 'pdb', 'pickle', 'pkgutil',
  'platform', 'plistlib', 'poplib', 'posix', 'pprint', 'profile', 'pstats', 'pty', 'pwd', 'py_compile', 'queue', 'quopri', 'random', 're',
  'readline', 'reprlib', 'resource', 'rlcompleter', 'runpy', 'sched', 'secrets', 'select', 'selectors', 'shelve', 'shlex', 'shutil', 'signal',
  'site', 'smtplib', 'socket', 'socketserver', 'sqlite3', 'ssl', 'stat', 'statistics', 'string', 'stringprep', 'struct', 'subprocess',
  'symtable', 'sys', 'sysconfig', 'syslog', 'tabnanny', 'tarfile', 'telnetlib', 'tempfile', 'termios', 'textwrap', 'threading', 'time',
  'timeit', 'tkinter', 'token', 'tokenize', 'tomllib', 'trace', 'traceback', 'tracemalloc', 'tty', 'turtle', 'types', 'typing', 'unicodedata',
  'unittest', 'urllib', 'uuid', 'venv', 'warnings', 'wave', 'weakref', 'webbrowser', 'winreg', 'winsound', 'wsgiref', 'xml', 'xmlrpc',
  'zipapp', 'zipfile', 'zipimport', 'zlib', 'zoneinfo', '_thread'])
const RUST_ROOTS = new Set(['crate', 'self', 'super', 'std', 'core', 'alloc'])
const JAVA_STDLIB = /^(?:java|javax|jdk|sun|kotlin|kotlinx|scala|groovy|android|androidx|dalvik)\./u

function collect(text: string, pattern: RegExp, style: ImportSpec['style'], out: ImportSpec[], group = 1): void {
  for (const match of text.matchAll(pattern)) {
    const spec = match[group]?.trim()
    if (spec !== undefined && spec !== '') out.push({ spec, style })
  }
}

/**
 * The imports a file names.
 * @param language Language id from {@link fileType}.
 * @param text File content; for notebooks, the code cells.
 * @returns Specifiers in file order (repeats included).
 */
export function parseImports(language: string, text: string): ImportSpec[] {
  const out: ImportSpec[] = []
  switch (language) {
    case 'typescript': case 'javascript': case 'vue': case 'svelte': case 'astro':
      collect(text, /\bfrom\s*['"]([^'"\n]{1,300})['"]/gu, 'path', out)
      collect(text, /(?:^|[;\s])import\s*['"]([^'"\n]{1,300})['"]/gmu, 'path', out)
      collect(text, /\b(?:require|import)\s*\(\s*['"]([^'"\n]{1,300})['"]\s*\)/gu, 'path', out)
      break
    case 'python': case 'sage': case 'jupyter':
      for (const match of text.matchAll(/^[ \t]*import[ \t]+([\w. \t,]+?)[ \t]*(?:#.*)?$/gmu)) {
        for (const part of match[1]!.split(',')) {
          const name = part.trim().split(/\s+as\s+/u)[0]!.trim()
          if (name !== '') out.push({ spec: name, style: 'module' })
        }
      }
      for (const match of text.matchAll(/^[ \t]*from[ \t]+(\.*[\w.]*)[ \t]+import[ \t]+\(?([^\n#)]*)/gmu)) {
        const names = match[2]!.split(',').map(part => part.trim().split(/\s+as\s+/u)[0]!.trim()).filter(name => /^\w+$/u.test(name))
        out.push({ spec: match[1]!, style: 'from', names })
      }
      if (language === 'sage') collect(text, /\b(?:load|attach)\(\s*['"]([^'"\n]+)['"]\s*\)/gu, 'path', out)
      break
    case 'c': case 'cpp': case 'objective-c': case 'cuda':
      collect(text, /^[ \t]*#[ \t]*(?:include|import)[ \t]*"([^"\n]+)"/gmu, 'include', out)
      break
    case 'go':
      collect(text, /^[ \t]*import[ \t]+(?:[\w.]+[ \t]+)?"([^"\n]+)"/gmu, 'module', out)
      for (const block of text.matchAll(/^[ \t]*import[ \t]*\(([\s\S]*?)\)/gmu)) collect(block[1]!, /"([^"\n]+)"/gu, 'module', out)
      break
    case 'rust':
      collect(text, /^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?mod[ \t]+(\w+)[ \t]*;/gmu, 'mod', out)
      collect(text, /^[ \t]*(?:pub(?:\([^)\n]*\))?[ \t]+)?use[ \t]+((?:\w+::)*\w+)/gmu, 'use', out)
      collect(text, /^[ \t]*extern[ \t]+crate[ \t]+(\w+)/gmu, 'use', out)
      break
    case 'java': case 'kotlin': case 'scala': case 'groovy':
      collect(text, /^[ \t]*import[ \t]+(?:static[ \t]+)?([\w.]+(?:\.\*|\._)?)[ \t]*;?[ \t]*$/gmu, 'class', out)
      break
    case 'php':
      collect(text, /\b(?:require|include)(?:_once)?\s*\(?\s*(?:__DIR__\s*\.\s*)?['"]([^'"\n]+)['"]/gu, 'path', out)
      collect(text, /^[ \t]*use[ \t]+\\?([\w\\]+)/gmu, 'class', out)
      break
    case 'ruby':
      collect(text, /^[ \t]*require_relative[ \t(]+['"]([^'"\n]+)['"]/gmu, 'path', out)
      collect(text, /^[ \t]*require[ \t(]+['"]([^'"\n]+)['"]/gmu, 'lib', out)
      break
    case 'lua':
      collect(text, /\brequire\s*\(?\s*['"]([^'"\n]+)['"]/gu, 'module', out)
      break
    case 'shell':
      for (const match of text.matchAll(/^[ \t]*(?:source|\.)[ \t]+["']?([^\s"';&|]+)/gmu)) {
        const raw = match[1]!
        // `"$(dirname "$0")/lib.sh"` and `$DIR/lib.sh` mean a file next to the script.
        const spec = raw.includes('$') ? `./${raw.slice(raw.lastIndexOf('/') + 1)}` : raw
        if (!spec.includes('$')) out.push({ spec, style: 'path' })
      }
      break
    case 'powershell':
      collect(text, /^[ \t]*\.[ \t]+["']?([^\s"']+\.ps1)/gmu, 'path', out)
      collect(text, /\bImport-Module[ \t]+["']?(\.[^\s"']+)/gu, 'path', out)
      break
    case 'html':
      collect(text, /<script\b[^>]*\bsrc\s*=\s*["']([^"']+)["']/giu, 'path', out)
      collect(text, /<link\b[^>]*\bhref\s*=\s*["']([^"']+\.css)["']/giu, 'path', out)
      break
    case 'css':
      collect(text, /@(?:import|use|forward)\s+(?:url\()?\s*['"]([^'"\n]+)['"]/gu, 'path', out)
      break
  }
  return out
}

/** Join a relative path to a directory; `undefined` when it leaves the project. */
export function joinPath(directory: string, relative: string): string | undefined {
  const parts = directory === '' ? [] : directory.split('/')
  for (const part of relative.replaceAll('\\', '/').split('/')) {
    if (part === '' || part === '.') continue
    if (part === '..') {
      if (parts.length === 0) return undefined
      parts.pop()
    } else parts.push(part)
  }
  return parts.join('/')
}

function dirname(path: string): string {
  const slash = path.lastIndexOf('/')
  return slash < 0 ? '' : path.slice(0, slash)
}

function find(context: ImportContext, candidates: readonly (string | undefined)[]): string | undefined {
  for (const candidate of candidates) {
    if (candidate === undefined || candidate === '') continue
    if (context.files.has(candidate)) return candidate
    const folded = context.lower.get(candidate.toLowerCase())
    if (folded !== undefined) return folded
  }
  return undefined
}

function jsCandidates(base: string): string[] {
  const candidates = [base, ...JS_EXTENSIONS.map(extension => base + extension), ...JS_EXTENSIONS.map(extension => `${base}/index${extension}`)]
  // TypeScript sources import their compiled `.js` names.
  const compiled = /\.(m|c)?jsx?$/u.exec(base)
  if (compiled !== null) {
    const stem = base.slice(0, -compiled[0].length)
    candidates.push(`${stem}.ts`, `${stem}.tsx`, `${stem}.${compiled[1] ?? ''}ts`)
  }
  return candidates
}

function firstFile(context: ImportContext, directory: string, pattern: RegExp): string | undefined {
  return context.directories.get(directory)?.find(path => pattern.test(path))
}

function resolveJs(context: ImportContext, from: string, spec: string): ImportTarget {
  const clean = spec.replace(/[?#].*$/u, '')
  if (/^(?:[a-z]+:)?\/\//iu.test(clean) || /^(?:data|virtual|https?):/iu.test(clean)) return { files: [] }
  if (clean.startsWith('.') || clean.startsWith('/')) {
    const base = clean.startsWith('/') ? joinPath('', clean) : joinPath(dirname(from), clean)
    const found = base === undefined ? undefined : find(context, jsCandidates(base))
    return { files: found === undefined ? [] : [found] }
  }
  for (const alias of context.aliases) {
    if (!clean.startsWith(alias.prefix)) continue
    for (const target of alias.targets) {
      const base = joinPath('', target + clean.slice(alias.prefix.length))
      const found = base === undefined ? undefined : find(context, jsCandidates(base))
      if (found !== undefined) return { files: [found] }
    }
  }
  if (clean.startsWith('node:')) return { files: [] }
  const parts = clean.split('/')
  const name = clean.startsWith('@') ? parts.slice(0, 2).join('/') : parts[0]!
  if (NODE_BUILTINS.has(name)) return { files: [] }
  return { files: [], external: name }
}

function pythonModule(context: ImportContext, roots: readonly string[], parts: readonly string[]): string | undefined {
  for (const root of roots) {
    for (let length = parts.length; length > 0; length--) {
      const base = joinPath(root, parts.slice(0, length).join('/'))
      if (base === undefined) continue
      const found = find(context, [`${base}.py`, `${base}/__init__.py`, `${base}.sage`, `${base}.pyi`])
      if (found !== undefined) return found
    }
  }
  return undefined
}

function resolvePython(context: ImportContext, from: string, import_: ImportSpec): ImportTarget {
  const spec = import_.spec
  if (spec.startsWith('.')) {
    const dots = /^\.+/u.exec(spec)![0].length
    let directory = dirname(from)
    for (let level = 1; level < dots; level++) directory = dirname(directory)
    const rest = spec.slice(dots)
    const files: string[] = []
    const base = rest === '' ? directory : joinPath(directory, rest.replaceAll('.', '/'))
    if (base === undefined) return { files }
    const inside = base === '' ? '' : `${base}/`
    for (const name of import_.names ?? []) {
      const found = find(context, [`${inside}${name}.py`, `${inside}${name}/__init__.py`])
      if (found !== undefined) files.push(found)
    }
    if (files.length === 0 || rest !== '') {
      const found = find(context, rest === '' ? [`${inside}__init__.py`] : [`${base}.py`, `${inside}__init__.py`])
      if (found !== undefined) files.push(found)
    }
    return { files }
  }
  const parts = spec.split('.').filter(Boolean)
  if (parts.length === 0) return { files: [] }
  const roots = [dirname(from), '', 'src', 'lib', 'python']
  if (import_.style === 'from') {
    const files: string[] = []
    for (const name of import_.names ?? []) {
      const found = pythonModule(context, roots, [...parts, name])
      if (found !== undefined && !found.endsWith(`${parts.join('/')}.py`) && !found.endsWith(`${parts.join('/')}/__init__.py`)) files.push(found)
    }
    const module = pythonModule(context, roots, parts)
    if (module !== undefined) files.push(module)
    if (files.length > 0) return { files: [...new Set(files)] }
  } else {
    const found = pythonModule(context, roots, parts)
    if (found !== undefined) return { files: [found] }
  }
  return PYTHON_STDLIB.has(parts[0]!) ? { files: [] } : { files: [], external: parts[0] }
}

function rustCrateRoot(context: ImportContext, from: string): string {
  let directory = dirname(from)
  for (;;) {
    if (find(context, [`${directory === '' ? '' : `${directory}/`}main.rs`, `${directory === '' ? '' : `${directory}/`}lib.rs`]) !== undefined) return directory
    if (directory === '') return dirname(from)
    directory = dirname(directory)
  }
}

function rustModuleDirectory(from: string): string {
  const name = from.slice(from.lastIndexOf('/') + 1)
  return /^(?:main|lib|mod)\.rs$/u.test(name) ? dirname(from) : `${dirname(from) === '' ? '' : `${dirname(from)}/`}${name.slice(0, -3)}`
}

function rustPath(context: ImportContext, directory: string, parts: readonly string[]): string | undefined {
  for (let length = parts.length; length > 0; length--) {
    const base = joinPath(directory, parts.slice(0, length).join('/'))
    if (base === undefined) continue
    const found = find(context, [`${base}.rs`, `${base}/mod.rs`])
    if (found !== undefined) return found
  }
  return undefined
}

function resolveRust(context: ImportContext, from: string, import_: ImportSpec): ImportTarget {
  if (import_.style === 'mod') {
    const base = joinPath(rustModuleDirectory(from), import_.spec)
    const found = base === undefined ? undefined : find(context, [`${base}.rs`, `${base}/mod.rs`])
    return { files: found === undefined ? [] : [found] }
  }
  const parts = import_.spec.split('::')
  const head = parts[0]!
  if (head === 'crate') {
    const found = rustPath(context, rustCrateRoot(context, from), parts.slice(1))
    return { files: found === undefined ? [] : [found] }
  }
  if (head === 'self' || head === 'super') {
    let directory = rustModuleDirectory(from)
    let rest = parts.slice(1)
    if (head === 'super') directory = dirname(directory)
    while (rest[0] === 'super') { directory = dirname(directory); rest = rest.slice(1) }
    const found = rustPath(context, directory, rest)
    return { files: found === undefined ? [] : [found] }
  }
  return RUST_ROOTS.has(head) ? { files: [] } : { files: [], external: head.replaceAll('_', '-') }
}

function resolveClass(context: ImportContext, language: string, spec: string): ImportTarget {
  const wildcard = /\.[*_]$/u.test(spec)
  const separator = language === 'php' ? '\\' : '.'
  const parts = (wildcard ? spec.slice(0, -2) : spec).split(separator).filter(Boolean)
  if (parts.length === 0) return { files: [] }
  if (wildcard) {
    // A wildcard import names a package directory.
    const suffix = `/${parts.join('/').toLowerCase()}`
    for (const [directory, members] of context.directories) {
      if (!`/${directory.toLowerCase()}`.endsWith(suffix)) continue
      const first = members.find(path => /\.(?:java|kt|scala|groovy)$/u.test(path))
      if (first !== undefined) return { files: [first] }
    }
  } else {
    // `import a.b.C.member` (static) names a class one level up.
    for (let drop = 0; drop < 2 && parts.length - drop > 0; drop++) {
      const kept = parts.slice(0, parts.length - drop)
      const found = context.classes.get(kept.join('/').toLowerCase())
        ?? (language === 'php' && kept.length > 1 ? context.classes.get(kept.slice(1).join('/').toLowerCase()) : undefined)
      if (found !== undefined) return { files: [found] }
    }
  }
  if (language === 'php' || JAVA_STDLIB.test(`${parts.join('.')}.`)) return { files: [] }
  const head = parts.slice(0, ['com', 'org', 'io', 'net', 'dev', 'me', 'cn'].includes(parts[0]!) ? 3 : 2)
  return { files: [], external: head.join('.') }
}

/**
 * Resolve one import of a file.
 * @param context Project files and settings.
 * @param language Language of the importing file.
 * @param from Path of the importing file.
 * @param import_ The import.
 * @returns Project files it leads to, or the external package it names.
 */
export function resolveImport(context: ImportContext, language: string, from: string, import_: ImportSpec): ImportTarget {
  const { spec, style } = import_
  switch (language) {
    case 'typescript': case 'javascript': case 'vue': case 'svelte': case 'astro':
      return resolveJs(context, from, spec)
    case 'python': case 'sage': case 'jupyter':
      if (style === 'path') {
        const found = find(context, [joinPath(dirname(from), spec)])
        return { files: found === undefined ? [] : [found] }
      }
      return resolvePython(context, from, import_)
    case 'c': case 'cpp': case 'objective-c': case 'cuda': {
      const local = find(context, [joinPath(dirname(from), spec), joinPath('', spec), joinPath('include', spec), joinPath('src', spec), joinPath('inc', spec)])
      if (local !== undefined) return { files: [local] }
      const clean = spec.replaceAll('\\', '/').replace(/^(?:\.\.?\/)+/u, '').toLowerCase()
      const matches = (context.basenames.get(clean.slice(clean.lastIndexOf('/') + 1)) ?? []).filter(path => `/${path.toLowerCase()}`.endsWith(`/${clean}`))
      return { files: matches.length === 1 ? matches : [] }
    }
    case 'go': {
      const module = context.goModule
      if (module !== undefined && (spec === module || spec.startsWith(`${module}/`))) {
        const directory = spec === module ? '' : spec.slice(module.length + 1)
        const found = firstFile(context, directory, /(?<!_test)\.go$/u)
        return { files: found === undefined ? [] : [found] }
      }
      const parts = spec.split('/')
      if (!parts[0]!.includes('.')) return { files: [] }
      return { files: [], external: parts.slice(0, parts[0] === 'gopkg.in' ? 2 : 3).join('/') }
    }
    case 'rust':
      return resolveRust(context, from, import_)
    case 'java': case 'kotlin': case 'scala': case 'groovy':
      return resolveClass(context, language, spec)
    case 'php':
      if (style === 'class') return resolveClass(context, language, spec)
      return { files: [find(context, [joinPath(dirname(from), spec), joinPath('', spec)])].filter((path): path is string => path !== undefined) }
    case 'ruby': {
      if (style === 'path') {
        const base = joinPath(dirname(from), spec)
        const found = base === undefined ? undefined : find(context, [base, `${base}.rb`])
        return { files: found === undefined ? [] : [found] }
      }
      const found = find(context, [`lib/${spec}.rb`, `${spec}.rb`, joinPath(dirname(from), `${spec}.rb`)])
      return found === undefined ? { files: [], external: spec.split('/')[0] } : { files: [found] }
    }
    case 'lua': {
      const path = spec.replaceAll('.', '/')
      const found = find(context, [`${path}.lua`, `${path}/init.lua`, joinPath(dirname(from), `${path}.lua`)])
      return { files: found === undefined ? [] : [found] }
    }
    case 'css': {
      if (/^(?:https?:|\/\/|data:)/iu.test(spec)) return { files: [] }
      const base = spec.startsWith('/') ? joinPath('', spec) : joinPath(dirname(from), spec)
      if (base === undefined) return { files: [] }
      const partial = `${dirname(base) === '' ? '' : `${dirname(base)}/`}_${base.slice(base.lastIndexOf('/') + 1)}`
      const found = find(context, [base, `${base}.scss`, `${partial}.scss`, `${base}.css`, `${partial}.css`, `${base}.sass`, `${base}.less`])
      return { files: found === undefined ? [] : [found] }
    }
    default: {
      if (/^(?:https?:|\/\/|data:)/iu.test(spec)) return { files: [] }
      const found = find(context, [spec.startsWith('/') ? joinPath('', spec) : joinPath(dirname(from), spec)])
      return { files: found === undefined ? [] : [found] }
    }
  }
}
