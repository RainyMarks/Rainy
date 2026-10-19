/**
 * Syntax highlighting for code cards, built on highlight.js. The highlighter and each grammar load on first use outside
 * the main bundle; until they arrive the code shows as plain text, then components using {@link useHighlightRevision}
 * highlight it.
 */
import { useSyncExternalStore, type CSSProperties } from 'react'
import type { HLJSApi, LanguageFn } from 'highlight.js'

type GrammarModule = { default: LanguageFn }
const LOADERS: Readonly<Record<string, () => Promise<GrammarModule>>> = {
  bash: () => import('highlight.js/lib/languages/bash'),
  c: () => import('highlight.js/lib/languages/c'),
  cpp: () => import('highlight.js/lib/languages/cpp'),
  css: () => import('highlight.js/lib/languages/css'),
  diff: () => import('highlight.js/lib/languages/diff'),
  go: () => import('highlight.js/lib/languages/go'),
  ini: () => import('highlight.js/lib/languages/ini'),
  java: () => import('highlight.js/lib/languages/java'),
  javascript: () => import('highlight.js/lib/languages/javascript'),
  json: () => import('highlight.js/lib/languages/json'),
  markdown: () => import('highlight.js/lib/languages/markdown'),
  php: () => import('highlight.js/lib/languages/php'),
  powershell: () => import('highlight.js/lib/languages/powershell'),
  python: () => import('highlight.js/lib/languages/python'),
  rust: () => import('highlight.js/lib/languages/rust'),
  sql: () => import('highlight.js/lib/languages/sql'),
  typescript: () => import('highlight.js/lib/languages/typescript'),
  x86asm: () => import('highlight.js/lib/languages/x86asm'),
  xml: () => import('highlight.js/lib/languages/xml'),
  yaml: () => import('highlight.js/lib/languages/yaml'),
}

/** Grammars that highlight embedded code with another grammar: scripts and styles in HTML, HTML in JSX and Markdown. */
const EMBEDDED: Readonly<Record<string, readonly string[]>> = {
  xml: ['css', 'javascript'], javascript: ['xml', 'css'], typescript: ['xml', 'css'], markdown: ['xml'],
}

const pending = new Map<string, Promise<void>>()
let revision = 0
const listeners = new Set<() => void>()
/** The highlighter, once its first grammar was asked for. */
let hljs: HLJSApi | undefined
let core: Promise<HLJSApi> | undefined

/**
 * Load a grammar once; components re-render through {@link useHighlightRevision} when it is registered.
 * @param name Grammar name from {@link LOADERS}.
 * @returns Completion of the load.
 */
export function loadGrammar(name: string): Promise<void> {
  const loader = LOADERS[name]
  if (loader === undefined || hljs?.getLanguage(name) !== undefined) return Promise.resolve()
  let task = pending.get(name)
  if (task === undefined) {
    core ??= import('highlight.js/lib/core').then(module => (hljs = module.default), (error: unknown) => { core = undefined; throw error })
    task = Promise.all([core, loader()]).then(([highlighter, module]) => {
      highlighter.registerLanguage(name, module.default)
      revision++
      for (const listener of listeners) listener()
    }, (error: unknown) => {
      pending.delete(name)
      console.error(`Grammar ${name} failed to load`, error)
    })
    pending.set(name, task)
    for (const embedded of EMBEDDED[name] ?? []) void loadGrammar(embedded)
  }
  return task
}

/** @returns A number that changes whenever a grammar finished loading; put it in the memo dependencies of highlighting. */
export function useHighlightRevision(): number {
  return useSyncExternalStore((listener) => { listeners.add(listener); return () => { listeners.delete(listener) } }, () => revision)
}

/**
 * Language hints and file extensions mapped to grammars: every alias the grammars register themselves (highlight.js
 * 11.11), which are unknown until a grammar loads, plus common extensions.
 */
const ALIASES: Readonly<Record<string, string>> = {
  ts: 'typescript', tsx: 'typescript', mts: 'typescript', cts: 'typescript',
  js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript', node: 'javascript',
  sh: 'bash', shell: 'bash', shellscript: 'bash', zsh: 'bash', console: 'bash',
  ps: 'powershell', ps1: 'powershell', psm1: 'powershell', pwsh: 'powershell',
  py: 'python', pyw: 'python', pyi: 'python', gyp: 'python', ipython: 'python',
  h: 'c', cc: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp', hxx: 'cpp', 'c++': 'cpp', 'h++': 'cpp',
  rs: 'rust', golang: 'go', yml: 'yaml', jsonc: 'json', jsonl: 'json', jsp: 'java',
  html: 'xml', htm: 'xml', xhtml: 'xml', svg: 'xml', rss: 'xml', atom: 'xml', xsd: 'xml', xsl: 'xml', xjb: 'xml', plist: 'xml', wsf: 'xml',
  md: 'markdown', mkd: 'markdown', mkdown: 'markdown', patch: 'diff',
  toml: 'ini', conf: 'ini', cfg: 'ini', properties: 'ini', env: 'ini',
  asm: 'x86asm', nasm: 'x86asm', s: 'x86asm',
}

/** One run of text with its token classes. */
export interface HighlightSpan {
  text: string
  /** highlight.js token classes (`hljs-keyword` …); styled by `theme/highlight.css`. */
  className?: string | undefined
  /** Inline style, used by ANSI terminal output. */
  style?: CSSProperties | undefined
}

/**
 * Resolve a language hint to a registered grammar.
 * @param lang Fence language, file extension or grammar name.
 * @returns The grammar name, or `undefined` for plain text.
 */
export function grammarFor(lang: string | undefined): string | undefined {
  if (lang === undefined) return undefined
  const key = lang.trim().toLowerCase()
  const name = ALIASES[key] ?? key
  return Object.hasOwn(LOADERS, name) ? name : undefined
}

/**
 * @param lang Language hint.
 * @returns Whether the hint has a grammar.
 */
export function supportsHighlighting(lang: string | undefined): boolean {
  return grammarFor(lang) !== undefined
}

/**
 * Pick a language from a file name.
 * @param path File path in either separator spelling.
 * @returns The grammar name, or `undefined`.
 */
export function languageForPath(path: string): string | undefined {
  const name = path.split(/[\\/]/).pop() ?? path
  const dot = name.lastIndexOf('.')
  return dot <= 0 ? undefined : grammarFor(name.slice(dot + 1))
}

const ENTITIES: Readonly<Record<string, string>> = { '&amp;': '&', '&lt;': '<', '&gt;': '>', '&quot;': '"', '&#x27;': "'", '&#39;': "'" }

/**
 * Highlight code into one span list per line.
 * @param code Source text.
 * @param lang Language hint.
 * @returns Lines of spans, or `undefined` when the language has no grammar or its grammar is still loading.
 */
export function highlightLines(code: string, lang: string | undefined): HighlightSpan[][] | undefined {
  const grammar = grammarFor(lang)
  if (grammar === undefined) return undefined
  if (hljs?.getLanguage(grammar) === undefined) { void loadGrammar(grammar); return undefined }
  const html = hljs!.highlight(code, { language: grammar, ignoreIllegals: true }).value
  const lines: HighlightSpan[][] = [[]]
  const classes: string[] = []
  const push = (text: string): void => {
    const parts = text.split('\n')
    parts.forEach((part, index) => {
      if (index > 0) lines.push([])
      if (part !== '') lines[lines.length - 1]!.push({ text: part, ...(classes.length ? { className: classes.join(' ') } : {}) })
    })
  }
  for (const match of html.matchAll(/<span class="([^"]*)">|<\/span>|([^<]+)/g)) {
    if (match[1] !== undefined) classes.push(match[1])
    else if (match[0] === '</span>') classes.pop()
    else push(match[2]!.replace(/&(?:amp|lt|gt|quot|#x27|#39);/g, entity => ENTITIES[entity] ?? entity))
  }
  return lines
}
