/** Build Rainy's thin carrier and profile plugins; upstream libraries are built by pnpm run build. */
import { build } from 'esbuild'
import { build as buildClient } from 'tsdown'
import { resolve } from 'node:path'
import { mkdir, copyFile, cp, symlink, lstat, readFile, rm } from 'node:fs/promises'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
const root = resolve(import.meta.dirname, '..')
const run = promisify(execFile)
const manifest: { version?: unknown } = JSON.parse(await readFile(resolve(root, 'package.json'), 'utf8'))
if (typeof manifest.version !== 'string' || !/^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(manifest.version)) {
  throw new Error('RainyAgent package.json must declare a valid application version')
}
process.env.DSH_CLIENT_VERSION = manifest.version
process.env.DSH_CLIENT_TITLE = 'RainyAgent'
const hostOnly = process.argv.includes('--host-only')
const release = process.argv.includes('--release')
if (release) await run(process.execPath, [resolve(root, 'scripts/release-signing-key.mjs')], { maxBuffer: 1024 * 1024 })
const releaseKeys: unknown = release ? JSON.parse(await readFile(resolve(root, 'runtime/release-public-keys.json'), 'utf8')) : null
const releaseDefines = { __RAINY_RELEASE_PUBLIC_KEYS__: JSON.stringify(releaseKeys) }
await run(process.execPath, [resolve(root, 'scripts/prepare-icesky-vendor.mjs'), '--verify'], { maxBuffer: 1024 * 1024 })
await run(process.execPath, [resolve(root, 'scripts/icesky-manifest.mjs')], { maxBuffer: 1024 * 1024 })
await run(process.execPath, [resolve(root, 'scripts/verify-ide-resources.mjs')], { maxBuffer: 1024 * 1024 })
await run(process.execPath, [resolve(root, 'scripts/verify-strata-runtime.mjs')], { maxBuffer: 1024 * 1024 })
if (!hostOnly) await run(process.execPath, [resolve(root, '../../node_modules/typescript/bin/tsc'), '-b',
  resolve(root, '../../packages/client/ui-sidebar-right/tsconfig.json'), resolve(root, '../../packages/client/ui-rainy/tsconfig.json')], { maxBuffer: 8 * 1024 * 1024 })
await mkdir(resolve(root, 'lib'), { recursive: true })
for (const name of ['license-guard.js', 'license-guard.js.map', 'setup/license.html', 'setup/license.js', 'setup/license.css']) {
  await rm(resolve(root, 'lib', name), { force: true })
}
const selfLink = resolve(root, 'node_modules/@deepseek-ai/dsh-rainy-desktop')
await mkdir(resolve(root, 'node_modules/@deepseek-ai'), { recursive: true })
try { await lstat(selfLink) } catch (error) {
  if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
  await symlink(root, selfLink, process.platform === 'win32' ? 'junction' : 'dir')
}
await build({ entryPoints: [resolve(root, 'src/main.ts')], outfile: resolve(root, 'lib/main.cjs'), bundle: true, platform: 'node', target: 'node22', format: 'cjs', external: ['electron'], define: releaseDefines, sourcemap: false, minify: release })
await build({ entryPoints: [resolve(root, 'src/preload.ts')], outfile: resolve(root, 'lib/preload.cjs'), bundle: true, platform: 'node', target: 'node22', format: 'cjs', external: ['electron'] })
await cp(resolve(root, 'src/setup'), resolve(root, 'lib/setup'), { recursive: true })
const testEntries = { 'composition-test': resolve(root, 'tests/composition.ts'), 'benchmark-test': resolve(root, 'tests/benchmark.mjs'), 'icesky-profile-test': resolve(root, 'tests/icesky-profile.ts') }
if (release) for (const name of [...Object.keys(testEntries).map(name => `${name}.js`), 'mcp-fixture.mjs', 'panel.js']) await rm(resolve(root, 'lib', name), { force: true })
await build({ entryPoints: { ...Object.fromEntries(['host', 'web', 'policy', 'compaction', 'extensions', 'ide', 'project-roots', 'runtime', 'runtime-bash', 'runtime-pwsh', 'project-memory'].map(name => [name, resolve(root, `src/${name}.ts`)])), ...(release ? {} : testEntries) }, outdir: resolve(root, 'lib'), bundle: true, platform: 'node', target: 'node22', format: 'esm', packages: 'external', define: releaseDefines, sourcemap: false, minify: release,
  plugins: [{ name: 'inline-private-source-helpers', setup(builder) {
    builder.onResolve({ filter: /cli\/src\/profile-boot\.ts$/ }, () => ({ path: resolve(root, '../cli/lib/types/profile-boot.js') }))
    builder.onResolve({ filter: /^[^./]/ }, (args) => {
      if (args.kind === 'entry-point') return
      if (args.path.startsWith('@deepseek-ai/dsh-compaction-basic/src/')) return { path: resolve(root, '../../packages/compaction/compaction-basic/lib/types', args.path.split('/src/')[1]!.replace(/\.ts$/, '.js')) }
      return { path: args.path, external: true }
    })
  } }],
})
if (!release) await copyFile(resolve(root, 'tests/fixtures/mcp-echo.mjs'), resolve(root, 'lib/mcp-fixture.mjs'))
const ui = resolve(root, '../../packages/client/ui-rainy')
const sidebar = resolve(root, '../../packages/client/ui-sidebar-right')
const layout = resolve(root, '../../packages/client/ui-layout')
if (!hostOnly) {
  for (const name of ['ui-primitives', 'ui-chat', 'ui-sidebar-files', 'ui-directory-picker-browse', 'ui-workspace', 'ui-conversation', 'ui-settings', 'ui-settings-general']) {
    const directory = resolve(root, `../../packages/client/${name}`)
    await buildClient({ cwd: directory, config: resolve(directory, 'tsdown.config.ts') })
  }
  await buildClient({ cwd: layout, config: resolve(layout, 'tsdown.config.ts') })
  await buildClient({ cwd: sidebar, config: resolve(sidebar, 'tsdown.config.ts') })
  await mkdir(resolve(ui, 'lib'), { recursive: true })
  await build({ entryPoints: [resolve(ui, 'src/index.ts')], outfile: resolve(ui, 'lib/index.js'), bundle: true, format: 'esm', platform: 'node' })
  await buildClient({ cwd: ui, config: resolve(ui, 'tsdown.config.ts') })
  await run(process.execPath, [resolve(root, 'scripts/build-editor.mjs')], { maxBuffer: 8 * 1024 * 1024 })
  await run(process.execPath, [resolve(root, '../web/node_modules/vite/bin/vite.js'), 'build'], {
    cwd: resolve(root, '../web'), maxBuffer: 8 * 1024 * 1024,
  })
}
console.log(hostOnly ? 'RainyAgent carrier and Host built; client assets were not rebuilt.' : 'RainyAgent carrier, profile plugins and editor assets built.')
