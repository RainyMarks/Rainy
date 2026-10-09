/** Explicit paths and read-only readiness for the shipped native development tools. */
import { createRequire } from 'node:module'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { readFile, stat } from 'node:fs/promises'
import type { SubprocessRuntime } from '@deepseek-ai/dsh-subprocess'

/** Resolved resource paths used by language, formatting, run and debug consumers. */
export interface IdeToolPaths {
  readonly resourceRoot: string
  readonly node: string
  readonly tsxImport: string
  readonly pyright: string
  readonly tsServer: string
  readonly prettier: string
  readonly ruff: string
}

/** Resolve the private runtime's dependencies before any execution request.
 * @returns absolute paths in the same execution world as the Host.
 */
export function getIdeToolPaths(): IdeToolPaths {
  const require = createRequire(import.meta.url)
  const resourceRoot = resolve(dirname(fileURLToPath(import.meta.url)), '../resources/ide')
  return {
    resourceRoot, node: process.execPath,
    tsxImport: require.resolve('tsx/esm'),
    pyright: join(dirname(require.resolve('pyright/package.json')), 'langserver.index.js'),
    tsServer: join(dirname(require.resolve('typescript-language-server/package.json')), 'lib/cli.mjs'),
    prettier: join(dirname(require.resolve('prettier/package.json')), 'bin/prettier.cjs'),
    ruff: join(resourceRoot, 'bin', process.platform === 'win32' ? 'ruff.exe' : 'ruff'),
  }
}

/** Availability of a configured tool; inspection never installs or runs project code. */
export interface IdeToolStatus {
  readonly platform: string
  readonly ubuntuVersion: string | null
  readonly offlineInstallSupported: boolean
  readonly tools: readonly { readonly name: string; readonly path: string | null; readonly ready: boolean }[]
}

/** Inspect compiler and runtime entrypoints without launching a program.
 * @param subprocess - execution-world executable lookup.
 * @param paths - already resolved bundled helpers.
 * @returns missing and ready entries, with the supported offline baseline.
 */
export async function inspectIdeTools(subprocess: Pick<SubprocessRuntime, 'resolveExecutable'>, paths: IdeToolPaths): Promise<IdeToolStatus> {
  let ubuntuVersion: string | null = null
  if (process.platform === 'linux') {
    const release = await readFile('/etc/os-release', 'utf8')
    if (/^ID=ubuntu$/m.test(release)) ubuntuVersion = /^VERSION_ID="?([^"\n]+)"?$/m.exec(release)?.[1] ?? null
  }
  const bundled = [
    ['node', paths.node], ['typescript-language-server', paths.tsServer], ['pyright', paths.pyright], ['ruff', paths.ruff],
    ['debugpy', join(paths.resourceRoot, 'python/debugpy/adapter/__main__.py')],
    ['js-debug', join(paths.resourceRoot, 'js-debug/src/dapDebugServer.js')],
  ] as const
  const tools: { name: string; path: string | null; ready: boolean }[] = await Promise.all(bundled.map(async ([name, path]) => {
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
