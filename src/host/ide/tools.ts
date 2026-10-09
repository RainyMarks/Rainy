/** Paths and read-only readiness of the development tools the IDE launches. */
import { lstatSync } from 'node:fs'
import { readFile, stat } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import type { IdeSubprocess } from './execution-process.ts'

/** Absolute tool paths used by language services, formatting, runs and debugging. */
export interface IdeToolPaths {
  /** `<resources>/ide`: ruff, debugpy and js-debug. */
  readonly resourceRoot: string
  readonly node: string
  readonly tsxImport: string
  readonly pyright: string
  readonly tsServer: string
  /** `tsserver.js` offered to the TypeScript language server when a project has no TypeScript of its own. */
  readonly tsserver: string
  readonly prettier: string
  readonly ruff: string
  /** Bundled ripgrep, when its platform package is installed. */
  readonly ripgrep: string | undefined
}

/**
 * Resolve the tool paths; npm packages are found next to the Host bundle.
 * @param resources The application's `resources` directory.
 * @returns Absolute paths in the Host's execution world.
 */
export async function getIdeToolPaths(resources: string): Promise<IdeToolPaths> {
  const require = createRequire(import.meta.url)
  const resourceRoot = join(resources, 'ide')
  let ripgrep: string | undefined
  try { ripgrep = (await import('@vscode/ripgrep')).rgPath } catch (_missingPlatformPackage) { ripgrep = undefined }
  return {
    resourceRoot,
    node: process.execPath,
    tsxImport: require.resolve('tsx/esm'),
    pyright: join(dirname(require.resolve('pyright/package.json')), 'langserver.index.js'),
    tsServer: join(dirname(require.resolve('typescript-language-server/package.json')), 'lib/cli.mjs'),
    tsserver: join(dirname(require.resolve('typescript')), 'tsserver.js'),
    prettier: join(dirname(require.resolve('prettier/package.json')), 'bin/prettier.cjs'),
    ruff: join(resourceRoot, 'bin', process.platform === 'win32' ? 'ruff.exe' : 'ruff'),
    ripgrep,
  }
}

/** Availability of each tool; inspection never installs or runs project code. */
export interface IdeToolStatus {
  readonly platform: string
  readonly ubuntuVersion: string | null
  readonly offlineInstallSupported: boolean
  readonly tools: readonly { readonly name: string; readonly path: string | null; readonly ready: boolean }[]
}

/**
 * Inspect compiler and runtime entrypoints without launching a program.
 * @param subprocess Executable lookup.
 * @param paths Resolved bundled tools.
 * @returns Missing and ready entries and whether the offline baseline applies.
 */
export async function inspectIdeTools(subprocess: Pick<IdeSubprocess, 'resolveExecutable'>, paths: IdeToolPaths): Promise<IdeToolStatus> {
  let ubuntuVersion: string | null = null
  if (process.platform === 'linux') {
    const release = await readFile('/etc/os-release', 'utf8')
    if (/^ID=ubuntu$/m.test(release)) ubuntuVersion = /^VERSION_ID="?([^"\n]+)"?$/m.exec(release)?.[1] ?? null
  }
  const bundled = [
    ['node', paths.node], ['typescript-language-server', paths.tsServer], ['pyright', paths.pyright], ['ruff', paths.ruff],
    ['debugpy', join(paths.resourceRoot, 'python/debugpy/adapter/__main__.py')],
    ['js-debug', join(paths.resourceRoot, 'js-debug/src/dapDebugServer.js')],
    ['ripgrep', paths.ripgrep],
  ] as const
  const tools: { name: string; path: string | null; ready: boolean }[] = await Promise.all(bundled.map(async ([name, path]) => {
    if (path === undefined) return { name, path: null, ready: false }
    try { return { name, path, ready: (await stat(path)).isFile() } }
    catch (error) {
      if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error
      return { name, path: null, ready: false }
    }
  }))
  for (const name of ['python3', 'gcc', 'g++', 'gdb', 'cmake', 'ninja', 'clangd', 'clang-format']) {
    try { tools.push({ name, path: await subprocess.resolveExecutable(name), ready: true }) }
    catch (_unavailableExecutable) { tools.push({ name, path: null, ready: false }) }
  }
  return { platform: process.platform, ubuntuVersion, offlineInstallSupported: process.platform === 'linux' && ubuntuVersion === '26.04', tools }
}

function spawnable(candidate: string): boolean {
  try {
    // lstat sees the Microsoft Store execution alias, which stat reports as inaccessible.
    const info = lstatSync(candidate)
    return info.isFile() || info.isSymbolicLink()
  } catch (_absentCandidate) {
    return false
  }
}

/**
 * Choose the PowerShell executable of the Windows IDE terminal.
 * @param configured PowerShell shipped with the Host (`RAINY_PWSH_PATH`); trusted as-is.
 * @param env Environment whose PATH and install locations are probed.
 * @returns PowerShell 7, a `pwsh.exe` on PATH, Windows PowerShell 5.1, or `pwsh` for a PATH lookup.
 */
export function resolvePwshPath(configured?: string, env: NodeJS.ProcessEnv = process.env): string {
  if (configured !== undefined && configured.length > 0) return configured
  if (process.platform !== 'win32') return 'pwsh'
  const candidates = [join(env.ProgramFiles ?? 'C:\\Program Files', 'PowerShell', '7', 'pwsh.exe')]
  for (const entry of (env.PATH ?? env.Path ?? '').split(';')) {
    const directory = entry.trim().replace(/^"|"$/g, '')
    if (directory !== '') candidates.push(join(directory, 'pwsh.exe'))
  }
  candidates.push(join(env.SystemRoot ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe'))
  return candidates.find(spawnable) ?? 'pwsh'
}
