/** Build the Electron shell, the Host bundle and the renderer (including the editor) into `dist/`. */
import { build } from 'esbuild'
import { spawn } from 'node:child_process'
import { existsSync } from 'node:fs'
import { cp, mkdir, readFile, rm } from 'node:fs/promises'
import { resolve } from 'node:path'
import hostExternals from './host-externals.json' with { type: 'json' }

const root = resolve(import.meta.dirname, '..')
const dist = resolve(root, 'dist')
const hostOnly = process.argv.includes('--host-only')
const release = process.argv.includes('--release')

/**
 * Run one Node script with inherited output.
 * @param script Script path relative to the repository root.
 * @param args Script arguments.
 * @returns Resolves when the script exits with code 0.
 */
function node(script: string, ...args: string[]): Promise<void> {
  return new Promise((accept, reject) => {
    const child = spawn(process.execPath, [resolve(root, script), ...args], { cwd: root, stdio: 'inherit' })
    child.once('error', reject)
    child.once('exit', (code, signal) => {
      if (code === 0) accept()
      else reject(new Error(`${script} failed with ${signal ?? `exit code ${String(code)}`}`))
    })
  })
}

async function main(): Promise<void> {
  const manifest: { version?: unknown } = JSON.parse(await readFile(resolve(root, 'package.json'), 'utf8'))
  if (typeof manifest.version !== 'string' || !/^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(manifest.version)) {
    throw new Error('RainyAgent package.json must declare a valid application version')
  }
  if (release) await node('scripts/release-signing-key.mjs')
  const releaseKeys: unknown = release ? JSON.parse(await readFile(resolve(root, 'runtime/release-public-keys.json'), 'utf8')) : null

  await node('scripts/prepare-icesky-vendor.mjs', '--verify')
  await node('scripts/icesky-manifest.mjs')
  // Release inputs are restored by bootstrap-release-inputs.mjs; a development checkout may not have them.
  for (const [directory, script] of [['resources/ide', 'scripts/verify-ide-resources.mjs'], ['resources/strata-runtime', 'scripts/verify-strata-runtime.mjs']] as const) {
    if (release || existsSync(resolve(root, directory))) await node(script)
    else console.log(`Skipped ${script}: ${directory} is not prepared.`)
  }

  await mkdir(dist, { recursive: true })
  await build({ entryPoints: [resolve(root, 'src/main/main.ts')], outfile: resolve(dist, 'main.cjs'), bundle: true, platform: 'node', target: 'node22',
    format: 'cjs', external: ['electron'], define: { __RAINY_RELEASE_PUBLIC_KEYS__: JSON.stringify(releaseKeys) }, sourcemap: false, minify: release })
  await build({ entryPoints: [resolve(root, 'src/preload/preload.ts')], outfile: resolve(dist, 'preload.cjs'), bundle: true, platform: 'node', target: 'node22',
    format: 'cjs', external: ['electron'], sourcemap: false, minify: release })
  await rm(resolve(dist, 'setup'), { recursive: true, force: true })
  await cp(resolve(root, 'src/setup'), resolve(dist, 'setup'), { recursive: true })
  console.log('Built dist/main.cjs, dist/preload.cjs and dist/setup.')

  // Bundled CommonJS dependencies call require(); native and process-launched packages stay in node_modules.
  await build({ entryPoints: [resolve(root, 'src/host/index.ts')], outfile: resolve(dist, 'host.js'), bundle: true, platform: 'node', target: 'node22',
    format: 'esm', splitting: false, external: ['electron', ...hostExternals], sourcemap: false, minify: release,
    banner: { js: "import { createRequire as __rainyCreateRequire } from 'node:module';\nconst require = __rainyCreateRequire(import.meta.url);" } })
  console.log('Built dist/host.js.')

  if (!hostOnly) {
    await node('node_modules/vite/bin/vite.js', 'build', '--config', resolve(root, 'vite.config.ts'))
  }
  console.log(hostOnly ? 'RainyAgent shell and Host built; the renderer was not rebuilt.' : 'RainyAgent shell, Host and renderer built.')
}

try { await main() }
catch (error) {
  // esbuild and the child scripts have already printed their diagnostics.
  console.error(error instanceof Error ? error.message : String(error))
  process.exitCode = 1
}
