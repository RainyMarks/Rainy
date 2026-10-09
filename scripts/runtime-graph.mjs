/**
 * Resolve the Host's production package closure for one target platform.
 *
 * The Host bundle (`dist/host.js`) keeps the packages in `host-externals.json` outside the bundle. This script walks
 * those packages and their dependencies through the pnpm store links in `node_modules/.pnpm`, drops packages whose
 * `os`/`cpu` exclude the target, and records target-only optional packages that are not installed on the build
 * machine as locked downloads (`platformDownloads`). `stage-windows.mjs` and `stage-linux.py` materialize the graph.
 *
 * `node scripts/runtime-graph.mjs [--windows]` writes `runtime/graph-windows.json` or `runtime/graph.json`.
 */
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, realpathSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import yaml from 'js-yaml'

/** Official Node.js release shipped with both Hosts. */
export const RUNTIME_NODE_VERSION = '22.22.1'
const ARCHITECTURE = 'x64'

/**
 * @param {string[] | undefined} values Package `os` or `cpu` list, possibly with `!` exclusions.
 * @param {string} target Target value.
 * @returns {boolean} Whether the list admits the target.
 */
function admits(values, target) {
  if (!Array.isArray(values) || values.length === 0) return true
  if (values.includes(`!${target}`)) return false
  const positive = values.filter(value => !value.startsWith('!'))
  return positive.length === 0 || positive.includes(target) || positive.includes('any')
}

/**
 * @param {{os?: string[], cpu?: string[], libc?: string[]}} manifest Package manifest or lockfile entry.
 * @param {'win32' | 'linux'} platform Target platform.
 * @returns {boolean} Whether the package can run on the x64 target (glibc on Linux).
 */
function supports(manifest, platform) {
  return admits(manifest.os, platform) && admits(manifest.cpu, ARCHITECTURE)
    && (platform !== 'linux' || !Array.isArray(manifest.libc) || manifest.libc.includes('glibc'))
}

/**
 * Locate one dependency the way Node resolves it from its owner's real directory.
 * @param {string} name Package name.
 * @param {string} owner Real directory of the depending package.
 * @returns {string | undefined} Real package directory, or undefined when it is not installed.
 */
function installedDirectory(name, owner) {
  const require = createRequire(join(owner, 'package.json'))
  for (const directory of require.resolve.paths(name) ?? []) {
    const candidate = join(directory, name)
    if (existsSync(join(candidate, 'package.json'))) return realpathSync(candidate)
  }
  return undefined
}

/**
 * Compute the production closure of the Host's external packages.
 * @param {{appDirectory: string, platform: 'win32' | 'linux'}} options Repository root and target platform.
 * @returns {{version: 2, root: string, platform: string, node: string, appVersion: string, packages: object[], platformDownloads: object[]}}
 *   Graph whose root item is the application; `source` paths are real directories on the build machine.
 */
export function resolveRuntimeGraph(options) {
  const app = realpathSync(options.appDirectory)
  const platform = options.platform
  const manifest = JSON.parse(readFileSync(join(app, 'package.json'), 'utf8'))
  const externals = JSON.parse(readFileSync(join(app, 'scripts/host-externals.json'), 'utf8'))
  const lock = yaml.load(readFileSync(join(app, 'pnpm-lock.yaml'), 'utf8'))
  const packages = new Map()
  const platformDownloads = []
  const id = directory => createHash('sha256').update(directory).digest('hex').slice(0, 16)

  /** @returns {object | undefined} The lockfile entry of a package that is not installed here. */
  function lockedEntry(name, range) {
    const candidates = Object.entries(lock.packages ?? {}).filter(([key]) => key.startsWith(`${name}@`))
    const exact = candidates.find(([key]) => key === `${name}@${range}`) ?? (candidates.length === 1 ? candidates[0] : undefined)
    if (!exact) return undefined
    return { version: exact[0].slice(name.length + 1), ...exact[1] }
  }

  function visit(directory) {
    if (packages.has(directory)) return packages.get(directory)
    const info = JSON.parse(readFileSync(join(directory, 'package.json'), 'utf8'))
    const item = { id: id(directory), name: info.name, version: info.version, source: directory, workspace: false, dependencies: {} }
    packages.set(directory, item)
    const optional = { ...info.optionalDependencies }
    for (const [name, meta] of Object.entries(info.peerDependenciesMeta ?? {})) if (meta?.optional) optional[name] ??= info.peerDependencies?.[name]
    const names = new Set([...Object.keys(info.dependencies ?? {}), ...Object.keys(info.peerDependencies ?? {}), ...Object.keys(info.optionalDependencies ?? {})])
    for (const name of names) {
      const isOptional = Object.hasOwn(optional, name)
      const child = installedDirectory(name, directory)
      if (child === undefined) {
        if (!isOptional) throw new Error(`Missing production dependency ${name} of ${info.name}`)
        const locked = lockedEntry(name, optional[name])
        // Packages for other platforms are not installed and never shipped; target-only packages are downloaded.
        if (locked === undefined || !supports(locked, platform) || (!locked.os && !locked.cpu)) continue
        const integrity = locked.resolution?.integrity
        if (typeof integrity !== 'string' || !integrity.startsWith('sha512-')) throw new Error(`No locked sha512 integrity for ${name}@${locked.version}`)
        platformDownloads.push({ owner: item.id, name, version: locked.version, integrity })
        continue
      }
      const target = JSON.parse(readFileSync(join(child, 'package.json'), 'utf8'))
      if (!supports(target, platform)) {
        if (isOptional) continue
        throw new Error(`${target.name} does not support ${platform}-${ARCHITECTURE} but ${info.name} requires it`)
      }
      item.dependencies[name] = visit(child).id
    }
    return item
  }

  const root = { id: id(app), name: manifest.name, version: manifest.version, source: app, workspace: true, dependencies: {} }
  packages.set(app, root)
  for (const name of externals) {
    const directory = installedDirectory(name, app)
    if (directory === undefined) throw new Error(`Host package ${name} is not installed; run pnpm install`)
    root.dependencies[name] = visit(directory).id
  }
  return { version: 2, root: root.id, platform, node: RUNTIME_NODE_VERSION, appVersion: manifest.version,
    packages: [...packages.values()], platformDownloads }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const app = resolve(import.meta.dirname, '..')
  const platform = process.argv.includes('--windows') ? 'win32' : 'linux'
  const graph = resolveRuntimeGraph({ appDirectory: app, platform })
  const output = resolve(app, 'runtime')
  mkdirSync(output, { recursive: true })
  writeFileSync(resolve(output, platform === 'win32' ? 'graph-windows.json' : 'graph.json'), JSON.stringify(graph, null, 2) + '\n')
  console.log(JSON.stringify({ platform, packages: graph.packages.length - 1, platformDownloads: graph.platformDownloads.map(item => `${item.name}@${item.version}`) }))
}
