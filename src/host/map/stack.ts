/** What a project is built with: its main languages, the frameworks and libraries it declares or imports, and build tools. */
import type { MapFile, MapPackage, MapStackItem } from '../../shared/map-protocol.ts'

const LANGUAGE_NAMES: Record<string, string> = {
  typescript: 'TypeScript', javascript: 'JavaScript', vue: 'Vue', svelte: 'Svelte', astro: 'Astro', python: 'Python', sage: 'SageMath',
  jupyter: 'Jupyter', c: 'C', cpp: 'C++', go: 'Go', rust: 'Rust', java: 'Java', kotlin: 'Kotlin', scala: 'Scala', groovy: 'Groovy',
  csharp: 'C#', fsharp: 'F#', vb: 'Visual Basic', swift: 'Swift', 'objective-c': 'Objective-C', dart: 'Dart', php: 'PHP', ruby: 'Ruby',
  lua: 'Lua', perl: 'Perl', r: 'R', julia: 'Julia', shell: 'Shell', powershell: 'PowerShell', batch: 'Batch', html: 'HTML', css: 'CSS',
  sql: 'SQL', assembly: 'Assembly', solidity: 'Solidity', zig: 'Zig', nim: 'Nim', verilog: 'Verilog', vhdl: 'VHDL', haskell: 'Haskell',
  ocaml: 'OCaml', elixir: 'Elixir', erlang: 'Erlang', clojure: 'Clojure', elm: 'Elm', wasm: 'WebAssembly', cuda: 'CUDA', glsl: 'GLSL',
  hlsl: 'HLSL', protobuf: 'Protocol Buffers', graphql: 'GraphQL',
}

/** Known packages: names as imported or declared (lower case; a trailing `/` or `.` matches a prefix), shown name, kind. */
const KNOWN: readonly [readonly string[], string, MapStackItem['kind']][] = [
  [['react', 'react-dom'], 'React', 'framework'], [['vue'], 'Vue', 'framework'], [['svelte'], 'Svelte', 'framework'],
  [['@angular/'], 'Angular', 'framework'], [['next'], 'Next.js', 'framework'], [['nuxt'], 'Nuxt', 'framework'],
  [['electron'], 'Electron', 'framework'], [['express'], 'Express', 'framework'], [['koa'], 'Koa', 'framework'],
  [['fastify'], 'Fastify', 'framework'], [['@nestjs/'], 'NestJS', 'framework'], [['@tauri-apps/'], 'Tauri', 'framework'],
  [['solid-js'], 'Solid', 'framework'], [['preact'], 'Preact', 'framework'], [['hono'], 'Hono', 'framework'],
  [['@codemirror/', 'codemirror'], 'CodeMirror', 'library'], [['@xterm/', 'xterm'], 'xterm.js', 'library'], [['monaco-editor'], 'Monaco', 'library'],
  [['zod'], 'Zod', 'library'], [['jquery'], 'jQuery', 'library'], [['three'], 'three.js', 'library'], [['d3'], 'D3', 'library'],
  [['socket.io', 'socket.io-client'], 'Socket.IO', 'library'], [['@prisma/client', 'prisma'], 'Prisma', 'library'],
  [['mongoose'], 'Mongoose', 'library'], [['sequelize'], 'Sequelize', 'library'], [['typeorm'], 'TypeORM', 'library'],
  [['redux', '@reduxjs/toolkit'], 'Redux', 'library'], [['axios'], 'Axios', 'library'], [['lodash'], 'Lodash', 'library'],
  [['tailwindcss'], 'Tailwind CSS', 'library'], [['antd'], 'Ant Design', 'library'], [['@mui/'], 'MUI', 'library'],
  [['element-plus'], 'Element Plus', 'library'], [['ws'], 'ws', 'library'], [['node-pty'], 'node-pty', 'library'],
  [['@modelcontextprotocol/'], 'MCP SDK', 'library'], [['openai'], 'OpenAI SDK', 'library'], [['@anthropic-ai/'], 'Anthropic SDK', 'library'],
  [['vite'], 'Vite', 'tool'], [['webpack'], 'webpack', 'tool'], [['esbuild'], 'esbuild', 'tool'], [['rollup'], 'Rollup', 'tool'],
  [['vitest'], 'Vitest', 'tool'], [['jest'], 'Jest', 'tool'], [['@playwright/test', 'playwright', 'playwright-core'], 'Playwright', 'tool'],
  [['puppeteer'], 'Puppeteer', 'library'], [['electron-builder'], 'electron-builder', 'tool'],
  [['flask'], 'Flask', 'framework'], [['django'], 'Django', 'framework'], [['fastapi'], 'FastAPI', 'framework'],
  [['tornado'], 'Tornado', 'framework'], [['streamlit'], 'Streamlit', 'framework'], [['gradio'], 'Gradio', 'framework'],
  [['pwn', 'pwntools'], 'pwntools', 'library'], [['crypto', 'cryptodome', 'pycryptodome', 'pycryptodomex'], 'PyCryptodome', 'library'],
  [['z3', 'z3-solver'], 'Z3', 'library'], [['sympy'], 'SymPy', 'library'], [['gmpy2'], 'gmpy2', 'library'], [['sage'], 'SageMath', 'library'],
  [['angr'], 'angr', 'library'], [['scapy'], 'Scapy', 'library'], [['capstone'], 'Capstone', 'library'], [['unicorn'], 'Unicorn', 'library'],
  [['keystone', 'keystone-engine'], 'Keystone', 'library'], [['libnum'], 'libnum', 'library'], [['ropgadget'], 'ROPgadget', 'tool'],
  [['numpy'], 'NumPy', 'library'], [['pandas'], 'pandas', 'library'], [['scipy'], 'SciPy', 'library'], [['matplotlib'], 'Matplotlib', 'library'],
  [['torch'], 'PyTorch', 'framework'], [['tensorflow'], 'TensorFlow', 'framework'], [['sklearn', 'scikit-learn'], 'scikit-learn', 'library'],
  [['transformers'], 'Transformers', 'library'], [['requests'], 'Requests', 'library'], [['httpx'], 'HTTPX', 'library'],
  [['aiohttp'], 'aiohttp', 'library'], [['bs4', 'beautifulsoup4'], 'Beautiful Soup', 'library'], [['selenium'], 'Selenium', 'library'],
  [['sqlalchemy'], 'SQLAlchemy', 'library'], [['pydantic'], 'Pydantic', 'library'], [['pil', 'pillow'], 'Pillow', 'library'],
  [['cv2', 'opencv-python'], 'OpenCV', 'library'], [['pytest'], 'pytest', 'tool'], [['click'], 'Click', 'library'], [['typer'], 'Typer', 'library'],
  [['github.com/gin-gonic/gin'], 'Gin', 'framework'], [['github.com/labstack/echo'], 'Echo', 'framework'], [['github.com/gofiber/fiber'], 'Fiber', 'framework'],
  [['github.com/spf13/cobra'], 'Cobra', 'library'], [['gorm.io/gorm'], 'GORM', 'library'], [['google.golang.org/grpc'], 'gRPC', 'library'],
  [['tokio'], 'Tokio', 'library'], [['serde'], 'Serde', 'library'], [['actix-web'], 'Actix Web', 'framework'], [['axum'], 'Axum', 'framework'],
  [['rocket'], 'Rocket', 'framework'], [['clap'], 'clap', 'library'], [['reqwest'], 'reqwest', 'library'], [['tauri'], 'Tauri', 'framework'],
  [['bevy'], 'Bevy', 'framework'], [['org.springframework.', 'org.springframework', 'spring-boot-starter', 'spring-boot-starter-web'], 'Spring', 'framework'],
  [['junit', 'org.junit.', 'org.junit'], 'JUnit', 'tool'], [['laravel/framework'], 'Laravel', 'framework'], [['symfony/'], 'Symfony', 'framework'],
  [['topthink/framework', 'topthink/think'], 'ThinkPHP', 'framework'], [['rails'], 'Rails', 'framework'], [['sinatra'], 'Sinatra', 'framework'],
]

/** Files whose presence names a tool. */
const TOOL_FILES: readonly [RegExp, string][] = [
  [/^(?:.*\/)?dockerfile(?:\.[^/]+)?$/iu, 'Docker'], [/^(?:.*\/)?(?:docker-)?compose\.ya?ml$/iu, 'Docker Compose'],
  [/^(?:.*\/)?cmakelists\.txt$/iu, 'CMake'], [/^(?:.*\/)?(?:gnu)?makefile$/iu, 'Make'], [/^\.github\/workflows\//iu, 'GitHub Actions'],
  [/^vite\.config\.[cm]?[jt]s$/iu, 'Vite'], [/^webpack\.config\.[cm]?[jt]s$/iu, 'webpack'], [/^electron-builder(?:\.config)?\.[a-z]+$/iu, 'electron-builder'],
  [/^(?:.*\/)?conftest\.py$|^pytest\.ini$/iu, 'pytest'], [/^vitest\.config\.[cm]?[jt]s$/iu, 'Vitest'], [/^jest\.config\.[cm]?[jt]s$/iu, 'Jest'],
  [/^playwright\.config\.[cm]?[jt]s$/iu, 'Playwright'], [/^(?:.*\/)?build\.gradle(?:\.kts)?$/iu, 'Gradle'], [/^(?:.*\/)?pom\.xml$/iu, 'Maven'],
  [/^(?:.*\/)?\.?gitlab-ci\.yml$/iu, 'GitLab CI'], [/^(?:.*\/)?pnpm-workspace\.yaml$/iu, 'pnpm'],
]

/** Manifests read for declared dependencies, by file name. */
const MANIFESTS = /(?:^|\/)(?:package\.json|requirements[\w.-]*\.txt|pyproject\.toml|pipfile|setup\.py|go\.mod|cargo\.toml|pom\.xml|build\.gradle(?:\.kts)?|composer\.json|gemfile)$/iu

function lookup(name: string): [string, MapStackItem['kind']] | undefined {
  const lower = name.toLowerCase()
  for (const [names, shown, kind] of KNOWN) {
    for (const candidate of names) {
      if (candidate.endsWith('/') || candidate.endsWith('.') ? lower.startsWith(candidate) : lower === candidate) return [shown, kind]
    }
  }
  return undefined
}

/**
 * Dependency names declared by one manifest.
 * @param path Manifest path.
 * @param text Its content.
 * @returns Package names as their ecosystem writes them.
 */
export function declaredDependencies(path: string, text: string): string[] {
  const name = path.slice(path.lastIndexOf('/') + 1).toLowerCase()
  const names: string[] = []
  const add = (value: string | undefined): void => {
    const clean = value?.trim()
    if (clean !== undefined && clean !== '' && clean.length <= 100) names.push(clean)
  }
  if (name === 'package.json' || name === 'composer.json') {
    try {
      const json = JSON.parse(text) as Record<string, unknown>
      for (const field of ['dependencies', 'devDependencies', 'peerDependencies', 'require', 'require-dev']) {
        const value = json[field]
        if (value !== null && typeof value === 'object') for (const key of Object.keys(value)) add(key)
      }
    } catch (_invalidJson) { /* A broken manifest declares nothing. */ }
  } else if (name.startsWith('requirements')) {
    for (const line of text.split('\n')) add(/^\s*([A-Za-z0-9][\w.-]*)/u.exec(line.replace(/#.*/u, ''))?.[1])
  } else if (name === 'pyproject.toml' || name === 'setup.py' || name === 'pipfile') {
    for (const match of text.matchAll(/["']([A-Za-z0-9][\w.-]*)\s*(?:\[[^\]]*\])?\s*(?:[<>=!~;][^"']*)?["']/gu)) add(match[1])
    if (name === 'pipfile') for (const match of text.matchAll(/^([A-Za-z0-9][\w.-]*)\s*=/gmu)) add(match[1])
  } else if (name === 'go.mod') {
    for (const match of text.matchAll(/^\s*(?:require\s+)?([a-z0-9.-]+\.[a-z]+\/[\w./-]+)\s+v/gmu)) add(match[1])
  } else if (name === 'cargo.toml') {
    let inDependencies = false
    for (const line of text.split('\n')) {
      const section = /^\s*\[([^\]]+)\]/u.exec(line)
      if (section !== null) { inDependencies = /dependencies$/u.test(section[1]!.trim()); continue }
      if (inDependencies) add(/^\s*([A-Za-z0-9_-]+)\s*=/u.exec(line)?.[1])
    }
  } else if (name === 'pom.xml') {
    for (const match of text.matchAll(/<dependency>[\s\S]*?<groupId>([^<]+)<\/groupId>[\s\S]*?<artifactId>([^<]+)<\/artifactId>/gu)) { add(match[1]); add(match[2]) }
  } else if (name.startsWith('build.gradle')) {
    for (const match of text.matchAll(/['"]([\w.-]+):([\w.-]+)(?::[^'"]*)?['"]/gu)) { add(match[1]); add(match[2]) }
  } else if (name === 'gemfile') {
    for (const match of text.matchAll(/^\s*gem\s+['"]([^'"]+)['"]/gmu)) add(match[1])
  }
  return names
}

/** @returns Whether a path names a dependency manifest the stack reads. */
export function isManifest(path: string): boolean {
  return MANIFESTS.test(path)
}

/**
 * The project's stack.
 * @param files Project files.
 * @param packages External packages and the files that import them.
 * @param declared Dependency names from the manifests.
 * @returns Languages first, then frameworks, libraries and tools.
 */
export function detectStack(files: readonly MapFile[], packages: readonly MapPackage[], declared: readonly string[]): MapStackItem[] {
  const languageLines = new Map<string, number>()
  let codeLines = 0
  for (const file of files) {
    if (file.kind !== 'code' || file.language === '' || LANGUAGE_NAMES[file.language] === undefined) continue
    languageLines.set(file.language, (languageLines.get(file.language) ?? 0) + file.lines)
    codeLines += file.lines
  }
  const languages = [...languageLines].sort((left, right) => right[1] - left[1])
    .filter(([, lines], index) => index === 0 || lines >= codeLines * 0.08).slice(0, 4)
    .map(([language]): MapStackItem => ({ name: LANGUAGE_NAMES[language]!, kind: 'language' }))
  const usage = new Map<string, { kind: MapStackItem['kind']; score: number }>()
  const note = (name: string, kind: MapStackItem['kind'], score: number): void => {
    const current = usage.get(name)
    usage.set(name, { kind: current?.kind ?? kind, score: (current?.score ?? 0) + score })
  }
  for (const item of packages) {
    const known = lookup(item.name)
    if (known !== undefined) note(known[0], known[1], 1 + item.files.length)
  }
  for (const name of declared) {
    const known = lookup(name)
    if (known !== undefined) note(known[0], known[1], 1)
  }
  for (const file of files) {
    for (const [pattern, tool] of TOOL_FILES) if (pattern.test(file.path)) note(tool, 'tool', 1)
  }
  const shown = new Set(languages.map(item => item.name))
  const rank = { language: 0, framework: 1, library: 2, tool: 3 }
  const rest = [...usage].filter(([name]) => !shown.has(name))
    .sort((left, right) => rank[left[1].kind] - rank[right[1].kind] || right[1].score - left[1].score || left[0].localeCompare(right[0]))
    .map(([name, { kind }]): MapStackItem => ({ name, kind }))
  // Without any known library, the most imported external packages say the most.
  if (!rest.some(item => item.kind === 'library' || item.kind === 'framework')) {
    const popular = [...packages].filter(item => item.files.length >= 2).sort((left, right) => right.files.length - left.files.length).slice(0, 4)
    rest.splice(0, 0, ...popular.map((item): MapStackItem => ({ name: item.name, kind: 'library' })))
  }
  return [...languages, ...rest].slice(0, 16)
}
