/** The Host runtime closure: external packages only, filtered and completed for each target platform. */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { resolveRuntimeGraph } from '../../scripts/runtime-graph.mjs'

const app = fileURLToPath(new URL('../..', import.meta.url))
const externals = JSON.parse(readFileSync(new URL('../../scripts/host-externals.json', import.meta.url), 'utf8'))
const host = process.platform === 'win32' ? 'win32' : 'linux'

for (const platform of ['win32', 'linux']) {
  test(`${platform} graph holds the external packages and their production dependencies`, () => {
    const graph = resolveRuntimeGraph({ appDirectory: app, platform })
    const byId = new Map(graph.packages.map(item => [item.id, item]))
    const root = byId.get(graph.root)
    assert.equal(root.workspace, true)
    assert.deepEqual(Object.keys(root.dependencies).sort(), [...externals].sort())
    const names = new Set(graph.packages.map(item => item.name))
    for (const bundled of ['electron', 'vite', 'react', '@earendil-works/pi-ai', 'ws', 'zod']) assert.equal(names.has(bundled), false, bundled)
    for (const item of graph.packages) for (const child of Object.values(item.dependencies)) assert(byId.has(child), `${item.name} -> ${child}`)
    // Each target receives its own ripgrep and esbuild binaries, installed here or downloaded from the lockfile.
    const other = platform === 'win32' ? 'linux' : 'win32'
    const shipped = new Set([...names, ...graph.platformDownloads.map(item => item.name)])
    for (const name of [`@vscode/ripgrep-${platform}-x64`, `@esbuild/${platform}-x64`]) assert(shipped.has(name), name)
    for (const name of [`@vscode/ripgrep-${other}-x64`, `@esbuild/${other}-x64`]) assert.equal(shipped.has(name), false, name)
    if (platform === host) assert.deepEqual(graph.platformDownloads, [])
    for (const download of graph.platformDownloads) assert.match(download.integrity, /^sha512-/u)
  })
}
