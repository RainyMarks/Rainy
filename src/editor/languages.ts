/** Lazily loaded CodeMirror language support, chosen by file extension and then by the IDE language id. */
import type { Extension } from '@codemirror/state'
import { StreamLanguage, type StreamParser } from '@codemirror/language'

type Loader = () => Promise<Extension>

function legacy(load: () => Promise<StreamParser<unknown>>): Loader {
  return async () => StreamLanguage.define(await load())
}

const loaders: Record<string, Loader> = {
  python: async () => (await import('@codemirror/lang-python')).python(),
  javascript: async () => (await import('@codemirror/lang-javascript')).javascript(),
  jsx: async () => (await import('@codemirror/lang-javascript')).javascript({ jsx: true }),
  typescript: async () => (await import('@codemirror/lang-javascript')).javascript({ typescript: true }),
  tsx: async () => (await import('@codemirror/lang-javascript')).javascript({ typescript: true, jsx: true }),
  c: async () => (await import('@codemirror/lang-cpp')).cpp(),
  cpp: async () => (await import('@codemirror/lang-cpp')).cpp(),
  json: async () => (await import('@codemirror/lang-json')).json(),
  markdown: async () => (await import('@codemirror/lang-markdown')).markdown(),
  html: async () => (await import('@codemirror/lang-html')).html(),
  css: async () => (await import('@codemirror/lang-css')).css(),
  yaml: async () => (await import('@codemirror/lang-yaml')).yaml(),
  php: async () => (await import('@codemirror/lang-php')).php(),
  sql: async () => (await import('@codemirror/lang-sql')).sql(),
  xml: async () => (await import('@codemirror/lang-xml')).xml(),
  rust: async () => (await import('@codemirror/lang-rust')).rust(),
  go: async () => (await import('@codemirror/lang-go')).go(),
  java: async () => (await import('@codemirror/lang-java')).java(),
  shell: legacy(async () => (await import('@codemirror/legacy-modes/mode/shell')).shell),
  powershell: legacy(async () => (await import('@codemirror/legacy-modes/mode/powershell')).powerShell),
  toml: legacy(async () => (await import('@codemirror/legacy-modes/mode/toml')).toml),
  lua: legacy(async () => (await import('@codemirror/legacy-modes/mode/lua')).lua),
  ruby: legacy(async () => (await import('@codemirror/legacy-modes/mode/ruby')).ruby),
  perl: legacy(async () => (await import('@codemirror/legacy-modes/mode/perl')).perl),
  dockerfile: legacy(async () => (await import('@codemirror/legacy-modes/mode/dockerfile')).dockerFile),
  diff: legacy(async () => (await import('@codemirror/legacy-modes/mode/diff')).diff),
  ini: legacy(async () => (await import('@codemirror/legacy-modes/mode/properties')).properties),
  cmake: legacy(async () => (await import('@codemirror/legacy-modes/mode/cmake')).cmake),
  x86: legacy(async () => (await import('@codemirror/legacy-modes/mode/gas')).gas),
}

const extensions: Record<string, string> = {
  py: 'python', pyi: 'python', pyw: 'python',
  js: 'javascript', mjs: 'javascript', cjs: 'javascript', jsx: 'jsx',
  ts: 'typescript', mts: 'typescript', cts: 'typescript', tsx: 'tsx',
  c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp', hxx: 'cpp', ino: 'cpp',
  json: 'json', jsonc: 'json', ipynb: 'json',
  md: 'markdown', markdown: 'markdown',
  html: 'html', htm: 'html', vue: 'html', svelte: 'html',
  css: 'css', scss: 'css', less: 'css',
  yml: 'yaml', yaml: 'yaml',
  php: 'php', sql: 'sql', xml: 'xml', svg: 'xml', xaml: 'xml', csproj: 'xml',
  rs: 'rust', go: 'go', java: 'java', kt: 'java', scala: 'java',
  sh: 'shell', bash: 'shell', zsh: 'shell',
  ps1: 'powershell', psm1: 'powershell', psd1: 'powershell',
  toml: 'toml', lua: 'lua', rb: 'ruby', pl: 'perl', pm: 'perl',
  diff: 'diff', patch: 'diff', ini: 'ini', cfg: 'ini', conf: 'ini', properties: 'ini', env: 'ini',
  cmake: 'cmake', s: 'x86', asm: 'x86', S: 'x86',
}

const names: Record<string, string> = { dockerfile: 'dockerfile', makefile: 'shell', 'cmakelists.txt': 'cmake', '.bashrc': 'shell', '.zshrc': 'shell' }

/**
 * Language key for a document.
 * @param path Editor path; its file name and extension decide first.
 * @param language IDE language id used when the path does not decide.
 * @returns A key of the loader table, or `undefined` for plain text.
 */
export function languageKey(path: string, language: string): string | undefined {
  const name = path.split(/[\\/]/).at(-1) ?? ''
  const byName = names[name.toLowerCase()]
  if (byName !== undefined) return byName
  const dot = name.lastIndexOf('.')
  const extension = dot < 0 ? '' : name.slice(dot + 1)
  return extensions[extension] ?? extensions[extension.toLowerCase()] ?? (language in loaders ? language : undefined)
}

const cache = new Map<string, Promise<Extension>>()

/**
 * Load language support.
 * @param key Key from `languageKey`.
 * @returns The CodeMirror extension; plain text when the key is unknown or loading fails.
 */
export function loadLanguage(key: string | undefined): Promise<Extension> {
  if (key === undefined) return Promise.resolve([])
  const loader = loaders[key]
  if (loader === undefined) return Promise.resolve([])
  let pending = cache.get(key)
  if (pending === undefined) {
    pending = loader().catch((error: unknown) => {
      cache.delete(key)
      console.error(`Language support for ${key} failed to load`, error)
      return []
    })
    cache.set(key, pending)
  }
  return pending
}

/**
 * LSP `languageId` for a document served by a language server.
 * @param key Key from `languageKey`.
 * @returns The id typescript-language-server, pyright and clangd expect.
 */
export function lspLanguageId(key: string | undefined): string | undefined {
  switch (key) {
    case 'python': return 'python'
    case 'javascript': return 'javascript'
    case 'jsx': return 'javascriptreact'
    case 'typescript': return 'typescript'
    case 'tsx': return 'typescriptreact'
    case 'c': return 'c'
    case 'cpp': return 'cpp'
    default: return undefined
  }
}

/**
 * Host language-server connection that serves a document.
 * @param key Key from `languageKey`.
 * @returns The `/rainy/ide/lsp` language, or `undefined` when no server applies.
 */
export function serverLanguage(key: string | undefined): 'python' | 'javascript' | 'typescript' | 'c' | 'cpp' | undefined {
  switch (key) {
    case 'python': return 'python'
    case 'javascript':
    case 'jsx': return 'javascript'
    case 'typescript':
    case 'tsx': return 'typescript'
    case 'c': return 'c'
    case 'cpp': return 'cpp'
    default: return undefined
  }
}
