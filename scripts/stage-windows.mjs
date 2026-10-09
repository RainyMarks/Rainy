/**
 * Assemble `runtime/windows-host/`, the native Windows Host shipped as an installer resource.
 *
 * Layout: `node/` (official Node.js), `app/` (package.json, dist/host.js, dist/renderer, resources, bin/rg.exe), `node_modules/`
 * (the production closure from `runtime/graph-windows.json`), `pwsh/` and `runtime.json` (file inventory). The
 * directory is replaced atomically; a failed run leaves the previous staging in place.
 */
import { createHash } from 'node:crypto'
import { createReadStream, createWriteStream, constants } from 'node:fs'
import { cp, mkdir, mkdtemp, readFile, readdir, rename, rm, stat, writeFile } from 'node:fs/promises'
import { dirname, join, resolve, relative, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { pipeline } from 'node:stream/promises'
import { Readable } from 'node:stream'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import * as tar from 'tar'

const app = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const graph = JSON.parse(await readFile(join(app, 'runtime/graph-windows.json'), 'utf8'))
if (graph.platform !== 'win32' || graph.version !== 2) throw new Error('Prepare the Windows dependency graph first: node scripts/runtime-graph.mjs --windows')
const packages = new Map(graph.packages.map(item => [item.id, item]))
const cache = join(app, 'runtime/downloads')
await mkdir(cache, { recursive: true })
const destination = join(app, 'runtime/windows-host')
const staging = await mkdtemp(join(app, 'runtime/windows-host-pending-'))
const run = promisify(execFile)

async function digest(path, algorithm = 'sha256', encoding = 'hex') {
  const hash = createHash(algorithm)
  for await (const chunk of createReadStream(path)) hash.update(chunk)
  return hash.digest(encoding)
}
async function download(url, path, matches) {
  try { if (await matches(path)) return } catch (error) { if (error.code !== 'ENOENT') throw error }
  const response = await fetch(url)
  if (!response.ok || !response.body) throw new Error(`Download failed: ${response.status} ${url}`)
  await pipeline(Readable.fromWeb(response.body), createWriteStream(path + '.pending'))
  if (!await matches(path + '.pending')) throw new Error(`Artifact checksum differs: ${url}`)
  await rename(path + '.pending', path)
}
function safeOwned(path) {
  const child = relative(join(app, 'runtime'), resolve(path))
  if (!child || child === '..' || child.startsWith(`..${sep}`) || child.includes(':')) throw new Error('Refusing to remove a directory outside Rainy runtime staging.')
}

const nodeName = `node-v${graph.node}-win-x64.zip`
const sums = await (await fetch(`https://nodejs.org/dist/v${graph.node}/SHASUMS256.txt`)).text()
const checksum = sums.split('\n').find(line => line.endsWith(`  ${nodeName}`))?.split(/\s+/u)[0]
if (!checksum || !/^[a-f0-9]{64}$/u.test(checksum)) throw new Error('Official Node checksum is unavailable.')
const archive = join(cache, nodeName)
await download(`https://nodejs.org/dist/v${graph.node}/${nodeName}`, archive, async path => await digest(path) === checksum)
const extraction = `$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.IO.Compression.FileSystem; `
  + `[IO.Compression.ZipFile]::ExtractToDirectory('${archive.replaceAll("'", "''")}', '${staging.replaceAll("'", "''")}')`
await run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', extraction], { windowsHide: true, timeout: 120000 })
await rename(join(staging, `node-v${graph.node}-win-x64`), join(staging, 'node'))
async function stripSourceMaps(directory) {
  safeOwned(directory)
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) await stripSourceMaps(path)
    else if (entry.isFile() && entry.name.endsWith('.map')) await rm(path)
  }
}
await stripSourceMaps(join(staging, 'node'))

/** The application tree holds only what the Host reads at run time. */
async function copyApp(target) {
  for (const required of ['dist/host.js', 'dist/renderer/index.html']) {
    try { await stat(join(app, required)) } catch (error) {
      if (error.code === 'ENOENT') throw new Error(`Build the application first (pnpm run build:release): ${required} is missing`)
      throw error
    }
  }
  await mkdir(join(target, 'dist'), { recursive: true })
  const manifest = JSON.parse(await readFile(join(app, 'package.json'), 'utf8'))
  await writeFile(join(target, 'package.json'), JSON.stringify({ name: manifest.name, productName: manifest.productName, version: manifest.version,
    private: true, type: 'module', license: manifest.license }, null, 2) + '\n')
  for (const name of ['LICENSE', 'LICENSE.RainyAgent', 'LICENSE.upstream', 'THIRD_PARTY_NOTICES.md']) await cp(join(app, name), join(target, name))
  await cp(join(app, 'dist/host.js'), join(target, 'dist/host.js'))
  await cp(join(app, 'dist/renderer'), join(target, 'dist/renderer'), { recursive: true, filter: file => !file.endsWith('.map') })
  // Strata is an optional module downloaded by the carrier, never part of the Host.
  await cp(join(app, 'resources'), join(target, 'resources'), { recursive: true, dereference: true,
    filter: file => !file.endsWith('.map') && resolve(file) !== resolve(app, 'resources/strata-runtime') })
}

const defaultPackages = new Map()
for (const dependency of Object.values(packages.get(graph.root).dependencies)) {
  const item = packages.get(dependency); defaultPackages.set(item.name, item.id)
}
for (const item of packages.values()) if (!defaultPackages.has(item.name) && item.id !== graph.root) defaultPackages.set(item.name, item.id)
const directories = new Map()
directories.set(graph.root, join(staging, 'app'))
for (const [name, id] of defaultPackages) directories.set(id, join(staging, 'node_modules', name))
const copied = new Set()
async function copyPackage(item, target) {
  if (copied.has(target)) return
  copied.add(target)
  if (item.id === graph.root) { await copyApp(target); return }
  await mkdir(target, { recursive: true })
  for (const entry of await readdir(item.source, { withFileTypes: true })) {
    if (['node_modules', '.git', 'tests', 'test'].includes(entry.name)) continue
    await cp(join(item.source, entry.name), join(target, entry.name), { recursive: true, dereference: true, mode: constants.COPYFILE_FICLONE,
      filter: file => !/\.(?:map|tsbuildinfo)$/u.test(file) })
  }
}
for (const [id, target] of directories) await copyPackage(packages.get(id), target)

async function materialize(item, target, scope, chain = new Set()) {
  const key = JSON.stringify([item.id, [...scope.entries()]])
  if (chain.has(key)) throw new Error(`Unresolvable production dependency cycle for ${item.name}`)
  const nextChain = new Set([...chain, key])
  const local = new Map(scope)
  const children = []
  for (const [name, id] of Object.entries(item.dependencies)) {
    if (id === item.id || scope.get(name) === id) continue
    const child = packages.get(id)
    const childDirectory = join(target, 'node_modules', name)
    await copyPackage(child, childDirectory)
    local.set(name, id)
    children.push([child, childDirectory])
  }
  for (const [child, childDirectory] of children) await materialize(child, childDirectory, local, nextChain)
}
for (const [id, directory] of directories) await materialize(packages.get(id), directory, defaultPackages)

// Windows-only packages are absent from a checkout installed on another platform; they come from the locked registry tarball.
for (const dependency of graph.platformDownloads) {
  const owner = directories.get(dependency.owner) ?? join(staging, 'node_modules', packages.get(dependency.owner).name)
  const target = join(owner, 'node_modules', dependency.name)
  const tarball = join(cache, `${dependency.name.replace('/', '_')}-${dependency.version}.tgz`)
  const metadata = await (await fetch(`https://registry.npmjs.org/${encodeURIComponent(dependency.name)}/${dependency.version}`)).json()
  await download(metadata.dist.tarball, tarball, async path => `sha512-${await digest(path, 'sha512', 'base64')}` === dependency.integrity)
  await mkdir(target, { recursive: true })
  await tar.x({ file: tarball, cwd: target, strip: 1, strict: true, preservePaths: false })
  // Nested copies of the owner find the platform package through the top-level node_modules.
  const hoisted = join(staging, 'node_modules', dependency.name)
  try { await stat(hoisted) } catch (error) {
    if (error.code !== 'ENOENT') throw error
    await cp(target, hoisted, { recursive: true })
  }
  console.log(`Verified Windows dependency: ${dependency.name}@${dependency.version}`)
}

// The Host puts <app>/bin on PATH for its shell tool.
await mkdir(join(staging, 'app/bin'), { recursive: true })
await cp(join(staging, 'node_modules/@vscode/ripgrep-win32-x64/bin/rg.exe'), join(staging, 'app/bin/rg.exe'))

// Windows resources are installed from their matching component; Linux ELF helpers cannot enter this tree.
safeOwned(staging)
await rm(join(staging, 'app/resources/ide/system-packages'), { recursive: true, force: true })
await rm(join(staging, 'app/resources/ide/ruff-x86_64-unknown-linux-gnu'), { recursive: true, force: true })
await rm(join(staging, 'app/resources/ide/bin/ruff'), { force: true })
try {
  await cp(join(app, 'runtime/component-stage/windows-basic/python/Scripts/ruff.exe'), join(staging, 'app/resources/ide/bin/ruff.exe'))
} catch (error) { if (error.code !== 'ENOENT') throw error }
await cp(join(app, 'runtime/component-stage/windows-basic/pwsh'), join(staging, 'pwsh'), { recursive: true, dereference: true })
await cp(join(app, 'runtime/component-stage/windows-basic/python/Lib/site-packages/debugpy'), join(staging, 'app/resources/ide/python/debugpy'), { recursive: true, dereference: true })
const files = []
async function inventory(directory) {
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) await inventory(path)
    else if (entry.isFile()) files.push({ path: relative(staging, path).split(sep).join('/'), bytes: (await stat(path)).size, sha256: await digest(path) })
    else throw new Error(`Nonportable runtime entry: ${path}`)
  }
}
await inventory(staging)
await writeFile(join(staging, 'runtime.json'), JSON.stringify({ version: 1, platform: 'win32', architecture: 'x64', node: graph.node,
  packages: [...packages.values()].map(item => ({ name: item.name, version: item.version })), files }, null, 2) + '\n')
safeOwned(destination)
const backup = destination + '.previous'
safeOwned(backup)
await rm(backup, { recursive: true, force: true })
try { await rename(destination, backup) } catch (error) { if (error.code !== 'ENOENT') throw error }
try { await rename(staging, destination) } catch (error) { await rename(backup, destination); throw error }
await rm(backup, { recursive: true, force: true })
console.log(JSON.stringify({ destination, packages: graph.packages.length, materializedDirectories: copied.size, files: files.length, bytes: files.reduce((sum, row) => sum + row.bytes, 0) }))
