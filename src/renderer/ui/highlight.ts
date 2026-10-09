/** Synchronous syntax highlighting for code cards, built on highlight.js with a fixed language set. */
import type { CSSProperties } from 'react'
import hljs from 'highlight.js/lib/core'
import bash from 'highlight.js/lib/languages/bash'
import c from 'highlight.js/lib/languages/c'
import cpp from 'highlight.js/lib/languages/cpp'
import css from 'highlight.js/lib/languages/css'
import diff from 'highlight.js/lib/languages/diff'
import go from 'highlight.js/lib/languages/go'
import ini from 'highlight.js/lib/languages/ini'
import java from 'highlight.js/lib/languages/java'
import javascript from 'highlight.js/lib/languages/javascript'
import json from 'highlight.js/lib/languages/json'
import markdown from 'highlight.js/lib/languages/markdown'
import php from 'highlight.js/lib/languages/php'
import powershell from 'highlight.js/lib/languages/powershell'
import python from 'highlight.js/lib/languages/python'
import rust from 'highlight.js/lib/languages/rust'
import sql from 'highlight.js/lib/languages/sql'
import typescript from 'highlight.js/lib/languages/typescript'
import x86asm from 'highlight.js/lib/languages/x86asm'
import xml from 'highlight.js/lib/languages/xml'
import yaml from 'highlight.js/lib/languages/yaml'

const LANGUAGES = { bash, c, cpp, css, diff, go, ini, java, javascript, json, markdown, php, powershell, python, rust, sql, typescript, x86asm, xml, yaml }
for (const [name, language] of Object.entries(LANGUAGES)) hljs.registerLanguage(name, language)

/** Language hints and file extensions mapped to registered grammars. */
const ALIASES: Readonly<Record<string, string>> = {
  ts: 'typescript', tsx: 'typescript', mts: 'typescript', cts: 'typescript',
  js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript', node: 'javascript',
  sh: 'bash', shell: 'bash', shellscript: 'bash', zsh: 'bash', console: 'bash',
  ps1: 'powershell', psm1: 'powershell', pwsh: 'powershell',
  py: 'python', pyw: 'python', pyi: 'python',
  h: 'c', cc: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp', hxx: 'cpp', 'c++': 'cpp',
  rs: 'rust', golang: 'go', yml: 'yaml', jsonc: 'json', jsonl: 'json',
  html: 'xml', htm: 'xml', svg: 'xml', md: 'markdown', patch: 'diff',
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
  return hljs.getLanguage(name) === undefined ? undefined : name
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
 * @returns Lines of spans, or `undefined` when the language has no grammar.
 */
export function highlightLines(code: string, lang: string | undefined): HighlightSpan[][] | undefined {
  const grammar = grammarFor(lang)
  if (grammar === undefined) return undefined
  const html = hljs.highlight(code, { language: grammar, ignoreIllegals: true }).value
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
