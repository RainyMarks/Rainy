/** What a file is by its name: language and kind, and which files are tests or generated locks. */
import type { MapFileKind } from '../../shared/map-protocol.ts'

/** Extensions of source code, by language id. */
const CODE: Record<string, string> = {
  ts: 'typescript', tsx: 'typescript', mts: 'typescript', cts: 'typescript',
  js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript',
  vue: 'vue', svelte: 'svelte', astro: 'astro',
  py: 'python', pyw: 'python', pyi: 'python', sage: 'sage', ipynb: 'jupyter',
  c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp', hxx: 'cpp', ino: 'cpp',
  go: 'go', rs: 'rust', java: 'java', kt: 'kotlin', kts: 'kotlin', scala: 'scala', groovy: 'groovy',
  cs: 'csharp', fs: 'fsharp', vb: 'vb', swift: 'swift', m: 'objective-c', mm: 'objective-c', dart: 'dart',
  php: 'php', rb: 'ruby', lua: 'lua', pl: 'perl', pm: 'perl', r: 'r', jl: 'julia',
  sh: 'shell', bash: 'shell', zsh: 'shell', fish: 'shell', ps1: 'powershell', psm1: 'powershell', bat: 'batch', cmd: 'batch',
  html: 'html', htm: 'html', css: 'css', scss: 'css', sass: 'css', less: 'css', sql: 'sql',
  asm: 'assembly', s: 'assembly', nasm: 'assembly', sol: 'solidity', zig: 'zig', nim: 'nim', v: 'verilog', sv: 'verilog', vhd: 'vhdl',
  hs: 'haskell', ml: 'ocaml', ex: 'elixir', exs: 'elixir', erl: 'erlang', clj: 'clojure', elm: 'elm',
  wat: 'wasm', proto: 'protobuf', graphql: 'graphql', gql: 'graphql', cu: 'cuda', glsl: 'glsl', hlsl: 'hlsl',
}

const DOC: Record<string, string> = { md: 'markdown', markdown: 'markdown', mdx: 'markdown', rst: 'rst', adoc: 'asciidoc', tex: 'latex', org: 'org' }

const CONFIG: Record<string, string> = {
  json: 'json', jsonc: 'json', json5: 'json', yaml: 'yaml', yml: 'yaml', toml: 'toml', ini: 'ini', cfg: 'ini', conf: 'ini',
  xml: 'xml', plist: 'xml', properties: 'ini', env: 'ini', gradle: 'gradle', cmake: 'cmake', mk: 'make', nix: 'nix', tf: 'terraform',
}

/** Names that carry no extension or whose extension says nothing. */
const NAMES: Record<string, [string, MapFileKind]> = {
  makefile: ['make', 'config'], gnumakefile: ['make', 'config'], dockerfile: ['docker', 'config'], 'cmakelists.txt': ['cmake', 'config'],
  'docker-compose.yml': ['yaml', 'config'], 'docker-compose.yaml': ['yaml', 'config'], vagrantfile: ['ruby', 'config'],
  gemfile: ['ruby', 'config'], rakefile: ['ruby', 'code'], procfile: ['', 'config'], justfile: ['', 'config'],
  'go.mod': ['go', 'config'], 'go.work': ['go', 'config'], pipfile: ['toml', 'config'], 'requirements.txt': ['', 'config'],
  'requirements-dev.txt': ['', 'config'], 'constraints.txt': ['', 'config'],
  '.gitignore': ['', 'config'], '.gitattributes': ['', 'config'], '.editorconfig': ['', 'config'], '.npmrc': ['', 'config'],
  '.env': ['', 'config'], 'license': ['', 'doc'], 'licence': ['', 'doc'], 'copying': ['', 'doc'], 'notice': ['', 'doc'],
  'readme': ['', 'doc'], 'changelog': ['', 'doc'], 'authors': ['', 'doc'],
}

/** Generated dependency locks: config, but their size says nothing about the project. */
const LOCKS = new Set(['package-lock.json', 'pnpm-lock.yaml', 'yarn.lock', 'bun.lockb', 'cargo.lock', 'poetry.lock', 'pipfile.lock',
  'go.sum', 'composer.lock', 'gemfile.lock', 'uv.lock', 'pdm.lock', 'flake.lock', 'packages.lock.json'])

/** Extensions of files that are not text, or not meant to be read as text. */
const BINARY = new Set(['png', 'jpg', 'jpeg', 'gif', 'bmp', 'ico', 'icns', 'webp', 'avif', 'tif', 'tiff', 'psd', 'svg',
  'mp3', 'wav', 'ogg', 'flac', 'm4a', 'aac', 'mid', 'mp4', 'webm', 'mov', 'avi', 'mkv', 'ttf', 'otf', 'woff', 'woff2', 'eot',
  'pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'zip', 'tar', 'gz', 'tgz', 'bz2', 'xz', '7z', 'rar', 'zst', 'jar', 'war', 'apk', 'ipa',
  'exe', 'dll', 'so', 'dylib', 'a', 'lib', 'o', 'obj', 'bin', 'elf', 'out', 'pyc', 'pyo', 'class', 'wasm', 'node', 'pdb', 'msi', 'deb', 'rpm',
  'img', 'iso', 'dmg', 'vmdk', 'qcow2', 'raw', 'dat', 'db', 'sqlite', 'sqlite3', 'pcap', 'pcapng', 'cap', 'mem', 'dmp', 'core', 'npy', 'npz', 'pt', 'pth', 'onnx', 'pkl'])

const TEST_PATH = /(?:^|\/)(?:tests?|__tests__|specs?|e2e|testing)\//iu
const TEST_NAME = /(?:\.(?:test|spec)\.[^.]+$|^test_.+\.py$|_test\.(?:py|go|rb|c|cpp|rs)$|Tests?\.(?:java|kt|cs|swift)$)/u

/** What the map knows about a file before reading it. */
export interface FileType {
  readonly language: string
  readonly kind: MapFileKind
  /** Its content is binary, so it is not read. */
  readonly binary: boolean
  /** A generated lock file: its lines are not counted. */
  readonly lock: boolean
}

/**
 * Classify a file by its path.
 * @param path Project-relative path with `/` separators.
 * @returns Language, kind and whether to read it.
 */
export function fileType(path: string): FileType {
  const name = path.slice(path.lastIndexOf('/') + 1)
  const lower = name.toLowerCase()
  if (LOCKS.has(lower)) return { language: '', kind: 'config', binary: false, lock: true }
  const named = NAMES[lower] ?? NAMES[lower.replace(/\.(?:md|txt|rst)$/u, '')]
  if (named !== undefined) return { language: named[0] === '' && /\.md$/u.test(lower) ? 'markdown' : named[0], kind: named[1], binary: false, lock: false }
  if (lower.startsWith('dockerfile')) return { language: 'docker', kind: 'config', binary: false, lock: false }
  const dot = lower.lastIndexOf('.')
  const extension = dot > 0 ? lower.slice(dot + 1) : ''
  const code = CODE[extension]
  if (code !== undefined) {
    const test = TEST_PATH.test(`/${path}`) || TEST_NAME.test(name)
    return { language: code, kind: test ? 'test' : 'code', binary: false, lock: false }
  }
  const doc = DOC[extension]
  if (doc !== undefined) return { language: doc, kind: 'doc', binary: false, lock: false }
  const config = CONFIG[extension]
  if (config !== undefined) return { language: config, kind: 'config', binary: false, lock: false }
  if (BINARY.has(extension)) return { language: extension === 'svg' ? 'svg' : '', kind: 'asset', binary: true, lock: false }
  return { language: extension === 'txt' || extension === 'log' || extension === 'csv' ? extension : '', kind: 'other', binary: false, lock: false }
}
