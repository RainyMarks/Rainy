/** Prepare the pinned Windows tool pack without running any bundled program. */
import { createHash, randomUUID } from 'node:crypto'
import { execFile } from 'node:child_process'
import { createReadStream, createWriteStream } from 'node:fs'
import { copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rename, rm, stat, writeFile } from 'node:fs/promises'
import { basename, dirname, isAbsolute, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { pipeline } from 'node:stream/promises'
import { promisify } from 'node:util'
import { unzipSync } from 'fflate'
import { extractBuildInput } from './bootstrap-release-inputs.mjs'

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const markerName = '.native-tools-stage'
const catalogName = 'tools/manifest.json'
const inventoryName = 'toolpack-files.json'
const markerContents = 'RainyAgent native tools stage v1\n'
const ignoredNames = new Set(['.git', '.svn', '__pycache__', '.ds_store', 'thumbs.db', 'desktop.ini'])
const ignoredExtensions = ['.pyc', '.pyo', '.tmp', '.log']
const execFileAsync = promisify(execFile)

/** Resolve a slash-separated relative file path; reject traversal, drives, and alternate streams.
 * @param {string} root Owned absolute directory.
 * @param {string} name Relative package path.
 * @returns {string} Absolute child path.
 */
export function childPath(root, name) {
  if (typeof name !== 'string' || !name || name.includes('\\') || name.includes(':') || name.includes('\0')) throw new Error(`Invalid package path: ${name}`)
  if (name.split('/').some(part => !part || part === '.' || part === '..')) throw new Error(`Invalid package path: ${name}`)
  const target = resolve(root, ...name.split('/'))
  const within = relative(root, target)
  if (!within || isAbsolute(within) || within === '..' || within.startsWith(`..${sep}`)) throw new Error(`Package path escapes root: ${name}`)
  return target
}

function belongsTo(name, roots) {
  return roots.some(root => name === root || name.startsWith(`${root}/`))
}

/** Check external JSON fields before they control filesystem access.
 * @param {object} definition Parsed source manifest.
 * @returns {object} The validated source manifest.
 */
export function validateDefinition(definition) {
  if (definition?.version !== 1 || !Array.isArray(definition.tools) || !Array.isArray(definition.runtimes)) throw new Error('Native tool source manifest must have version 1, tools, and runtimes.')
  if (definition.sourceLayout && definition.sourceLayout !== 'ctf-all-in-one-dist') throw new Error('Unknown native tool source layout.')
  const ids = new Set()
  const runtimeRoots = definition.runtimes.flatMap(runtime => [...(runtime.copies ?? []).map(copy => copy.to), ...(runtime.archives ?? []).map(archive => archive.to)])
  for (const runtime of definition.runtimes) {
    for (const archive of runtime.archives ?? []) {
      const pinned = archive.sha512 === undefined && /^[a-f0-9]{64}$/.test(archive.sha256)
        || archive.sha256 === undefined && /^[a-f0-9]{128}$/.test(archive.sha512)
      if (archive.format !== 'zip' || !pinned || new URL(archive.url).protocol !== 'https:') throw new Error(`Unpinned runtime archive: ${runtime.id}`)
      if (archive.from !== undefined) childPath(appRoot, archive.from)
    }
  }
  for (const root of runtimeRoots) {
    childPath(appRoot, root)
    if (!root.startsWith('runtime/windows/')) throw new Error(`Runtime root must be below runtime/windows: ${root}`)
  }
  for (const tool of definition.tools) {
    if (!/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(tool.id) || ids.has(tool.id)) throw new Error(`Duplicate or invalid native tool id: ${tool.id}`)
    ids.add(tool.id)
    if (!['web', 'misc', 'reverse'].includes(tool.category) || typeof tool.name !== 'string' || !tool.name || typeof tool.version !== 'string') throw new Error(`Invalid native tool identity: ${tool.id}`)
    if (!tool.description?.zh || !tool.description?.en || !Array.isArray(tool.keywords) || !tool.keywords.every(value => typeof value === 'string')) throw new Error(`Invalid native tool description: ${tool.id}`)
    if (!Array.isArray(tool.roots) || !tool.roots.includes(`tools/${tool.id}`)) throw new Error(`Missing owned tool root: ${tool.id}`)
    for (const root of tool.roots) {
      childPath(appRoot, root)
      if (root !== `tools/${tool.id}` && !runtimeRoots.includes(root)) throw new Error(`Unregistered tool root: ${root}`)
    }
    const entries = [tool.entry, ...(tool.variants ?? []).map(variant => variant.entry)]
    if ((tool.variants ?? []).some(variant => tool.id !== 'x64dbg' || variant.id !== 'x32')) throw new Error(`Unregistered tool variant: ${tool.id}`)
    for (const entry of entries) {
      if (!entry || !['gui', 'console', 'java', 'web'].includes(entry.kind)) throw new Error(`Invalid entry kind: ${tool.id}`)
      if (entry.requiredFiles !== undefined && (!Array.isArray(entry.requiredFiles) || !entry.requiredFiles.every(value => typeof value === 'string'))) throw new Error(`Invalid required entry files: ${tool.id}`)
      for (const value of [entry.path, entry.cwd, ...(entry.runtime ? [entry.runtime] : []), ...(entry.dotnetRoot ? [entry.dotnetRoot] : []), ...(entry.pythonRoot ? [entry.pythonRoot] : []), ...(entry.requiredFiles ?? [])]) {
        childPath(appRoot, value)
        if (!belongsTo(value, tool.roots)) throw new Error(`Entry is outside registered roots: ${tool.id}`)
      }
      if (entry.kind === 'java' && !entry.runtime) throw new Error(`Java entry requires its packaged runtime: ${tool.id}`)
      if (entry.args && (!Array.isArray(entry.args) || !entry.args.every(value => typeof value === 'string' && !value.includes('\0')))) throw new Error(`Invalid entry arguments: ${tool.id}`)
    }
    for (const value of tool.preserve ?? []) {
      childPath(appRoot, value)
      if (!belongsTo(value, [`tools/${tool.id}`])) throw new Error(`Preserved data is outside its tool root: ${tool.id}`)
    }
    for (const item of tool.downloads ?? []) {
      childPath(appRoot, item.path)
      if (!belongsTo(item.path, [`tools/${tool.id}`]) || !/^[a-f0-9]{64}$/.test(item.sha256) || new URL(item.url).protocol !== 'https:') throw new Error(`Unpinned native tool download: ${tool.id}`)
    }
    if (tool.generatedFiles !== undefined && !Array.isArray(tool.generatedFiles)) throw new Error(`Invalid generated tool files: ${tool.id}`)
    for (const file of tool.generatedFiles ?? []) {
      childPath(appRoot, file.path)
      if (!belongsTo(file.path, [`tools/${tool.id}`]) || typeof file.content !== 'string' || file.content.includes('\0')) throw new Error(`Invalid generated tool file: ${tool.id}`)
    }
    for (const archive of tool.archives ?? []) {
      childPath(appRoot, archive.to)
      if (!['zip', 'tar.gz', 'nsis-7z'].includes(archive.format) || !belongsTo(archive.to, [`tools/${tool.id}`]) || !/^[a-f0-9]{64}$/.test(archive.sha256) || new URL(archive.url).protocol !== 'https:') throw new Error(`Unpinned native tool archive: ${tool.id}`)
      if (archive.from !== undefined) childPath(appRoot, archive.from)
      if (archive.include !== undefined) {
        if (!Array.isArray(archive.include)) throw new Error(`Invalid native archive inclusion list: ${tool.id}`)
        for (const path of archive.include) childPath(appRoot, path)
      }
      for (const file of archive.requiredFiles ?? []) {
        childPath(appRoot, file.path)
        if (!/^[a-f0-9]{64}$/.test(file.sha256)) throw new Error(`Unpinned required archive file: ${tool.id}`)
      }
    }
  }
  for (const owner of [...definition.runtimes, ...definition.tools]) {
    for (const copy of owner.copies ?? []) {
      childPath(appRoot, copy.from)
      childPath(appRoot, copy.to)
      const roots = owner.roots ?? runtimeRoots.filter(root => root === copy.to)
      if (!belongsTo(copy.to, roots)) throw new Error(`Copy destination is outside owned roots: ${owner.id}`)
      if (copy.sha256 && !/^[a-f0-9]{64}$/.test(copy.sha256)) throw new Error(`Invalid source SHA-256: ${owner.id}`)
      for (const excluded of copy.exclude ?? []) childPath(appRoot, excluded)
      for (const included of copy.include ?? []) childPath(appRoot, included)
    }
  }
  return definition
}

/** Hash one file at a time to avoid concurrent full-pack reads.
 * @param {string} path File to read.
 * @returns {Promise<string>} Lowercase SHA-256 digest.
 */
export async function sha256File(path) {
  return hashFile(path, 'sha256')
}

async function hashFile(path, algorithm) {
  const hash = createHash(algorithm)
  for await (const chunk of createReadStream(path)) hash.update(chunk)
  return hash.digest('hex')
}

async function maybeStat(path) {
  try { return await lstat(path) } catch (error) {
    if (error.code === 'ENOENT') return undefined
    throw error
  }
}

async function rejectLink(path) {
  const info = await lstat(path)
  if (info.isSymbolicLink()) throw new Error(`Symbolic links and junctions are not allowed in tool packs: ${path}`)
  return info
}

async function assertExistingParents(root, path) {
  const within = relative(root, path)
  if (isAbsolute(within) || within === '..' || within.startsWith(`..${sep}`)) throw new Error(`Path escapes owned root: ${path}`)
  let parent = root
  await rejectLink(root)
  for (const component of within.split(sep).filter(Boolean)) {
    parent = resolve(parent, component)
    const info = await maybeStat(parent)
    if (!info) break
    if (info.isSymbolicLink()) throw new Error(`Symbolic links and junctions are not allowed in tool packs: ${parent}`)
  }
}

function exclusion(name, rules) {
  const lower = name.toLowerCase()
  if (name.split('/').some(part => ignoredNames.has(part.toLowerCase()) || part.startsWith('._'))) return 'Generated cache or filesystem metadata'
  if (ignoredExtensions.some(extension => lower.endsWith(extension))) return 'Generated cache, temporary file, or log'
  const match = rules.find(rule => lower === rule.toLowerCase() || lower.startsWith(`${rule.toLowerCase()}/`))
  return match ? `Source exclusion: ${match}` : undefined
}

/** Enumerate only the selected source trees and retain exclusions as reviewable evidence.
 * @param {object} definition Validated source manifest.
 * @param {string} sourceRoot Local distribution directory, or its libdll directory for compatibility.
 * @returns {Promise<{files: object[], excluded: object[]}>} Copy candidates and exclusions.
 */
export async function planCopies(definition, sourceRoot) {
  sourceRoot = resolveSourceRoot(definition, sourceRoot)
  await rejectLink(sourceRoot)
  const files = []
  const excluded = []
  const destinations = new Set()
  for (const owner of [...definition.runtimes, ...definition.tools]) {
    for (const copy of owner.copies ?? []) {
      const source = childPath(sourceRoot, copy.from)
      await assertExistingParents(sourceRoot, source)
      async function visit(path, local, target) {
        const name = local || copy.from.split('/').at(-1)
        if (local && copy.include && !copy.include.some(included => local === included || local.startsWith(`${included}/`) || included.startsWith(`${local}/`))) {
          excluded.push({ owner: owner.id, path: `${copy.from}/${local}`, reason: 'Outside the selected application files' })
          return
        }
        const reason = exclusion(name, copy.exclude ?? [])
        if (reason) { excluded.push({ owner: owner.id, path: `${copy.from}${local ? `/${local}` : ''}`, reason }); return }
        const info = await rejectLink(path)
        if (info.isDirectory()) {
          if (copy.sha256) throw new Error(`A pinned source must be a file: ${copy.from}`)
          const entries = await readdir(path, { withFileTypes: true })
          entries.sort((a, b) => a.name.localeCompare(b.name, 'en'))
          for (const entry of entries) await visit(resolve(path, entry.name), local ? `${local}/${entry.name}` : entry.name, `${target}/${entry.name}`)
        } else if (info.isFile()) {
          const key = target.toLowerCase()
          if (destinations.has(key)) throw new Error(`Duplicate package destination: ${target}`)
          destinations.add(key)
          files.push({ owner: owner.id, source: path, path: target, bytes: info.size, ...(copy.sha256 ? { expectedSha256: copy.sha256 } : {}) })
        } else throw new Error(`Unsupported source file type: ${path}`)
      }
      await visit(source, '', copy.to)
    }
  }
  return { files, excluded }
}

function resolveSourceRoot(definition, sourceRoot) {
  const path = resolve(sourceRoot)
  return definition.sourceLayout === 'ctf-all-in-one-dist' && basename(path).toLowerCase() === 'libdll' ? dirname(path) : path
}

async function ensureOwnedStage(stageRoot, sourceRoot) {
  if (stageRoot.split(sep).some(part => part.toLowerCase() === 'resources')) throw new Error('Native tool staging must stay outside resources.')
  const sourceWithinStage = relative(stageRoot, sourceRoot)
  const stageWithinSource = relative(sourceRoot, stageRoot)
  if (!sourceWithinStage || (!isAbsolute(sourceWithinStage) && !sourceWithinStage.startsWith(`..${sep}`) && sourceWithinStage !== '..') || (!isAbsolute(stageWithinSource) && !stageWithinSource.startsWith(`..${sep}`) && stageWithinSource !== '..')) throw new Error('Source and stage roots must be separate non-overlapping directories.')
  const existing = await maybeStat(stageRoot)
  if (existing?.isSymbolicLink()) throw new Error('Native tool stage cannot be a symbolic link or junction.')
  for (let parent = dirname(stageRoot); dirname(parent) !== parent; parent = dirname(parent)) {
    if ((await maybeStat(parent))?.isSymbolicLink()) throw new Error(`Native tool stage parent cannot be a symbolic link or junction: ${parent}`)
  }
  await mkdir(stageRoot, { recursive: true })
  const marker = childPath(stageRoot, markerName)
  if (await maybeStat(marker)) {
    await rejectLink(marker)
    if (await readFile(marker, 'utf8') !== markerContents) throw new Error('Native tool stage marker is invalid.')
  } else {
    if ((await readdir(stageRoot)).length) throw new Error('Refusing to modify a nonempty directory without a native tool stage marker.')
    await writeFile(marker, markerContents, { flag: 'wx' })
  }
}

async function downloadPinned(item, cacheRoot, offline) {
  const algorithm = item.sha512 ? 'sha512' : 'sha256'
  const expected = item[algorithm]
  const target = childPath(cacheRoot, `${item.sha512 ? 'sha512-' : ''}${expected}.download`)
  await mkdir(cacheRoot, { recursive: true })
  await assertExistingParents(cacheRoot, target)
  if (await maybeStat(target)) {
    if (await hashFile(target, algorithm) === expected) return target
    throw new Error(`Cached download failed ${algorithm} verification: ${item.url}`)
  }
  if (offline) throw new Error(`Pinned download is absent from the offline cache: ${item.url}`)
  const partial = `${target}.${randomUUID()}.part`
  try {
    const response = await fetch(item.url, { signal: AbortSignal.timeout(300_000) })
    if (!response.ok || !response.body) throw new Error(`Cannot download ${item.url}: HTTP ${response.status}`)
    await pipeline(response.body, createWriteStream(partial, { flags: 'wx' }))
    if (await hashFile(partial, algorithm) !== expected) throw new Error(`Publisher ${algorithm} mismatch: ${item.url}`)
    await rename(partial, target)
  } finally {
    await rm(partial, { force: true })
  }
  return target
}

async function addRuntimeArchives(definition, options, cacheRoot, plan, temporaryRoots) {
  for (const runtime of definition.runtimes) {
    for (const archive of runtime.archives ?? []) {
      options.progress?.(`Checking pinned ${runtime.id}`)
      const downloaded = await downloadPinned(archive, cacheRoot, options.offline ?? false)
      const unpackRoot = await mkdtemp(resolve(cacheRoot, 'unpack-'))
      temporaryRoots.push(unpackRoot)
      let selectedFiles = 0
      for (const file of await extractZip(await readFile(downloaded), unpackRoot, 'runtime')) {
        if (archive.from && !file.name.startsWith(`${archive.from}/`)) continue
        const selected = archive.from ? file.name.slice(archive.from.length + 1) : file.name
        const path = `${archive.to}/${selected}`
        if (plan.files.some(entry => entry.path.toLowerCase() === path.toLowerCase())) throw new Error(`Duplicate package destination: ${path}`)
        plan.files.push({ owner: runtime.id, source: file.target, path, bytes: file.bytes })
        selectedFiles++
      }
      if (archive.from && !selectedFiles) throw new Error(`Runtime archive directory is missing: ${runtime.id}/${archive.from}`)
    }
  }
}

/** Extract a ZIP under one owned root, refusing escaping and case-colliding entries.
 * @param {Uint8Array} bytes ZIP archive.
 * @param {string} root Owned extraction directory.
 * @param {string} label Archive kind named in duplicate-entry errors.
 * @returns {Promise<{ name: string, target: string, bytes: number }[]>} Extracted files in archive order.
 */
async function extractZip(bytes, root, label) {
  const names = new Set()
  const files = []
  for (const [name, contents] of Object.entries(unzipSync(bytes))) {
    const directory = name.endsWith('/')
    const normalized = directory ? name.slice(0, -1) : name
    const target = childPath(root, normalized)
    if (names.has(normalized.toLowerCase())) throw new Error(`Duplicate ${label} ZIP entry: ${name}`)
    names.add(normalized.toLowerCase())
    if (directory) { await mkdir(target, { recursive: true }); continue }
    await mkdir(dirname(target), { recursive: true })
    await writeFile(target, contents, { flag: 'wx' })
    files.push({ name: normalized, target, bytes: contents.length })
  }
  return files
}

/** Validate a 7-Zip technical listing before extracting its files.
 * @param {string} listing Output from 7-Zip l -slt -ba.
 * @param {string} root Owned extraction directory.
 * @returns {string[]} Normalized relative entry names.
 */
export function validateArchiveListing(listing, root) {
  const names = new Set()
  for (const record of listing.trim().split(/\r?\n\r?\n/)) {
    const lines = record.split(/\r?\n/)
    const paths = lines.filter(line => line.startsWith('Path = '))
    if (paths.length !== 1 || lines.some(line => /^(?:Symbolic Link|Hard Link|Reparse) = /.test(line)) || lines.some(line => /^Attributes = .*\bl[rwx-]{9}\b/.test(line))) throw new Error('Unsupported native archive entry.')
    const name = paths[0].slice(7).replaceAll('\\', '/')
    childPath(root, name)
    if (names.has(name.toLowerCase())) throw new Error(`Duplicate native archive entry: ${name}`)
    names.add(name.toLowerCase())
  }
  return [...names]
}

async function addToolArchives(definition, options, cacheRoot, plan, temporaryRoots) {
  for (const tool of definition.tools) {
    for (const archive of tool.archives ?? []) {
      if (archive.format === 'nsis-7z' && !options.sevenZip) throw new Error('Pass --seven-zip <7z executable> to extract pinned NSIS tool archives.')
      options.progress?.(`Checking pinned ${tool.id} archive`)
      const downloaded = await downloadPinned(archive, cacheRoot, options.offline ?? false)
      const unpackRoot = await mkdtemp(resolve(cacheRoot, 'unpack-'))
      temporaryRoots.push(unpackRoot)
      const appFiles = childPath(unpackRoot, 'application')
      await mkdir(appFiles)
      if (archive.format === 'zip') {
        await extractZip(await readFile(downloaded), appFiles, 'native')
      } else if (archive.format === 'tar.gz') {
        await extractBuildInput(downloaded, appFiles)
      } else {
        const environment = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/KEY|SECRET|TOKEN|PASSWORD/i.test(key)))
        const run = (args) => execFileAsync(options.sevenZip, args, { windowsHide: true, timeout: 300_000, maxBuffer: 8 * 1024 * 1024, encoding: 'utf8', env: environment })
        const payloadName = '$PLUGINSDIR/app-64.7z'
        const outer = await run(['l', '-slt', '-ba', '-sccUTF-8', downloaded, payloadName])
        const outerNames = validateArchiveListing(outer.stdout, unpackRoot)
        if (outerNames.length !== 1 || outerNames[0] !== payloadName.toLowerCase()) throw new Error('NSIS archive must contain exactly one app-64.7z payload.')
        await run(['x', '-y', `-o${unpackRoot}`, downloaded, payloadName])
        const payload = childPath(unpackRoot, payloadName)
        await assertExistingParents(unpackRoot, payload)
        const inner = await run(['l', '-slt', '-ba', '-sccUTF-8', payload])
        validateArchiveListing(inner.stdout, appFiles)
        await run(['x', '-y', `-o${appFiles}`, payload])
      }
      const selectedRoot = archive.from ? childPath(appFiles, archive.from) : appFiles
      for (const required of archive.requiredFiles ?? []) {
        const path = childPath(selectedRoot, required.path)
        await assertExistingParents(selectedRoot, path)
        if (await sha256File(path) !== required.sha256) throw new Error(`Required archive file SHA-256 mismatch: ${tool.id}/${required.path}`)
      }
      const extracted = await planCopies({ tools: [{ id: tool.id, copies: [{ from: archive.from ? `application/${archive.from}` : 'application', to: archive.to, ...(archive.include ? { include: archive.include } : {}) }] }], runtimes: [] }, unpackRoot)
      for (const file of extracted.files) {
        if (plan.files.some(existing => existing.path.toLowerCase() === file.path.toLowerCase())) throw new Error(`Duplicate package destination: ${file.path}`)
        plan.files.push(file)
      }
      plan.excluded.push(...extracted.excluded)
    }
  }
}

async function removeTemporaryRoots(cacheRoot, temporaryRoots) {
  for (const root of temporaryRoots) {
    const within = relative(cacheRoot, root)
    if (!within || isAbsolute(within) || within.includes(sep) || !within.startsWith('unpack-')) throw new Error('Refusing to remove a directory outside the runtime extraction cache.')
    await assertExistingParents(cacheRoot, root)
    await rm(root, { recursive: true, force: true })
  }
}

async function addGeneratedFiles(definition, cacheRoot, plan, temporaryRoots) {
  const files = definition.tools.flatMap(tool => (tool.generatedFiles ?? []).map(file => ({ ...file, owner: tool.id })))
  if (!files.length) return
  const destinations = new Set(plan.files.map(file => file.path.toLowerCase()))
  for (const file of files) {
    const key = file.path.toLowerCase()
    if (destinations.has(key)) throw new Error(`Duplicate package destination: ${file.path}`)
    destinations.add(key)
  }
  await mkdir(cacheRoot, { recursive: true })
  await rejectLink(cacheRoot)
  const generatedRoot = await mkdtemp(resolve(cacheRoot, 'unpack-generated-'))
  temporaryRoots.push(generatedRoot)
  for (const file of files) {
    const source = childPath(generatedRoot, file.path)
    const content = Buffer.from(file.content, 'utf8')
    await mkdir(dirname(source), { recursive: true })
    await writeFile(source, content, { flag: 'wx' })
    plan.files.push({ owner: file.owner, source, path: file.path, bytes: content.length })
  }
}

async function copyVerified(item, stageRoot) {
  const before = await rejectLink(item.source)
  const digest = await sha256File(item.source)
  if (item.expectedSha256 && digest !== item.expectedSha256) throw new Error(`Pinned source SHA-256 mismatch: ${item.path}`)
  const after = await stat(item.source)
  if (before.size !== after.size || before.mtimeMs !== after.mtimeMs) throw new Error(`Source changed while hashing: ${item.source}`)
  const destination = childPath(stageRoot, item.path)
  await assertExistingParents(stageRoot, destination)
  const existing = await maybeStat(destination)
  if (existing?.isFile() && existing.size === after.size && await sha256File(destination) === digest) return { path: item.path, bytes: after.size, sha256: digest, copied: false }
  if (existing && !existing.isFile()) throw new Error(`Tool file destination is not a regular file: ${item.path}`)
  await mkdir(dirname(destination), { recursive: true })
  const partial = `${destination}.${randomUUID()}.part`
  try {
    await copyFile(item.source, partial)
    if (await sha256File(partial) !== digest) throw new Error(`Copied file differs from source: ${item.path}`)
    await rename(partial, destination)
  } finally {
    await rm(partial, { force: true })
  }
  return { path: item.path, bytes: after.size, sha256: digest, copied: true }
}

async function stageFiles(root) {
  const result = []
  async function walk(directory, prefix) {
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const name = prefix ? `${prefix}/${entry.name}` : entry.name
      const path = childPath(root, name)
      const info = await rejectLink(path)
      if (info.isDirectory()) await walk(path, name)
      else if (info.isFile()) result.push(name)
      else throw new Error(`Unsupported staged file type: ${name}`)
    }
  }
  await walk(root, '')
  return result
}

/** Verify every packaged file and reject unlisted additions.
 * @param {string} stageRoot Prepared package root.
 * @returns {Promise<object>} Verified file inventory.
 */
export async function verifyPack(stageRoot) {
  await rejectLink(stageRoot)
  await assertExistingParents(stageRoot, childPath(stageRoot, inventoryName))
  const inventory = JSON.parse(await readFile(childPath(stageRoot, inventoryName), 'utf8'))
  if (inventory.version !== 1 || inventory.algorithm !== 'sha256' || !Array.isArray(inventory.files)) throw new Error('Native tool file inventory is invalid.')
  const expected = new Set([markerName, inventoryName])
  for (const item of inventory.files) {
    if (expected.has(item.path) || !/^[a-f0-9]{64}$/.test(item.sha256)) throw new Error(`Invalid file inventory entry: ${item.path}`)
    expected.add(item.path)
    const path = childPath(stageRoot, item.path)
    await assertExistingParents(stageRoot, path)
    const info = await rejectLink(path)
    if (!info.isFile() || info.size !== item.bytes || await sha256File(path) !== item.sha256) throw new Error(`Native tool SHA-256 verification failed: ${item.path}`)
  }
  for (const name of await stageFiles(stageRoot)) if (!expected.has(name)) throw new Error(`Unlisted file in native tool stage: ${name}`)
  const catalog = JSON.parse(await readFile(childPath(stageRoot, catalogName), 'utf8'))
  if (catalog.version !== 1 || !Array.isArray(catalog.tools) || catalog.tools.length !== inventory.toolCount) throw new Error('Native tool catalog does not match the file inventory.')
  return inventory
}

/** Copy selected trees incrementally and produce a runtime catalog plus complete SHA-256 list.
 * @param {object} options Source definition, source/stage/cache paths, optional sevenZip extractor, offline flag, and progress callback.
 * @returns {Promise<object>} Preparation counts, exclusions, and catalog.
 */
export async function preparePack(options) {
  const definition = validateDefinition(options.definition)
  const selectedSourceRoot = resolveSourceRoot(definition, options.sourceRoot)
  await rejectLink(selectedSourceRoot)
  const sourceRoot = await realpath(selectedSourceRoot)
  const stageRoot = resolve(options.stageRoot)
  const cacheRoot = resolve(options.cacheRoot)
  await ensureOwnedStage(stageRoot, sourceRoot)
  const plan = await planCopies(definition, sourceRoot)
  const temporaryRoots = []
  try {
    await addRuntimeArchives(definition, options, cacheRoot, plan, temporaryRoots)
    await addToolArchives(definition, options, cacheRoot, plan, temporaryRoots)
    const unavailable = new Map()
    for (const tool of definition.tools) {
      for (const download of tool.downloads ?? []) {
        options.progress?.(`Checking pinned ${tool.id}`)
        try {
          const source = await downloadPinned(download, cacheRoot, options.offline ?? false)
          plan.files.push({ owner: tool.id, source, path: download.path, bytes: (await stat(source)).size })
        } catch (error) {
          unavailable.set(tool.id, error.message)
          options.progress?.(`${tool.id} unavailable: ${error.message}`)
        }
      }
    }
    await addGeneratedFiles(definition, cacheRoot, plan, temporaryRoots)
    const destinations = new Set()
    for (const item of plan.files) {
      const key = item.path.toLowerCase()
      if (destinations.has(key)) throw new Error(`Duplicate package destination: ${item.path}`)
      destinations.add(key)
    }
    const desired = new Set(plan.files.map(item => item.path))
    for (const name of await stageFiles(stageRoot)) {
      if ([markerName, catalogName, inventoryName].includes(name) || desired.has(name)) continue
      throw new Error(`Unlisted file must be reviewed before reusing this stage: ${name}`)
    }
    const files = []
    let copied = 0
    let lastOwner
    for (const item of plan.files) {
      if (lastOwner !== item.owner) { options.progress?.(`Preparing ${item.owner}`); lastOwner = item.owner }
      const verified = await copyVerified(item, stageRoot)
      if (verified.copied) copied++
      files.push({ path: verified.path, bytes: verified.bytes, sha256: verified.sha256 })
    }
    const byPath = new Map(files.map(item => [item.path, item]))
    const tools = definition.tools.map(({ copies, downloads, archives, generatedFiles, ...tool }) => {
      for (const entry of [tool.entry, ...(tool.variants ?? []).map(variant => variant.entry)]) {
        if ((!byPath.has(entry.path) || (entry.runtime && !byPath.has(entry.runtime))) && !unavailable.has(tool.id)) throw new Error(`Packaged entry or runtime is missing: ${tool.id}`)
        if (entry.dotnetRoot && !byPath.has(`${entry.dotnetRoot}/dotnet.exe`)) throw new Error(`Packaged .NET host is missing: ${tool.id}`)
        if (entry.pythonRoot && (!byPath.has(`${entry.pythonRoot}/python.exe`) || !byPath.has(`${entry.pythonRoot}/python3.dll`))) throw new Error(`Packaged Python runtime is missing: ${tool.id}`)
        for (const path of entry.requiredFiles ?? []) if (!byPath.has(path) && !unavailable.has(tool.id)) throw new Error(`Packaged required entry file is missing: ${tool.id}/${path}`)
      }
      const owned = files.filter(file => belongsTo(file.path, tool.roots))
      return { ...tool, available: !unavailable.has(tool.id), ...(unavailable.has(tool.id) ? { unavailableReason: unavailable.get(tool.id) } : { entrySha256: byPath.get(tool.entry.path).sha256 }), fileCount: owned.length, bytes: owned.reduce((sum, file) => sum + file.bytes, 0) }
    })
    const catalog = { version: 1, tools }
    const catalogBytes = Buffer.from(JSON.stringify(catalog, null, 2) + '\n')
    await mkdir(dirname(childPath(stageRoot, catalogName)), { recursive: true })
    await writeFile(childPath(stageRoot, catalogName), catalogBytes)
    files.push({ path: catalogName, bytes: catalogBytes.length, sha256: createHash('sha256').update(catalogBytes).digest('hex') })
    files.sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0)
    const inventory = { version: 1, algorithm: 'sha256', toolCount: tools.length, availableToolCount: tools.filter(tool => tool.available).length, sourceManifestSha256: createHash('sha256').update(JSON.stringify(definition)).digest('hex'), bytes: files.reduce((sum, file) => sum + file.bytes, 0), files }
    await writeFile(childPath(stageRoot, inventoryName), JSON.stringify(inventory, null, 2) + '\n')
    return { fileCount: files.length, bytes: inventory.bytes, copied, reused: plan.files.length - copied, excluded: plan.excluded, unavailable: [...unavailable].map(([id, reason]) => ({ id, reason })), catalog }
  } finally {
    await removeTemporaryRoots(cacheRoot, temporaryRoots)
  }
}

async function main() {
  const args = process.argv.slice(2)
  const value = (name, fallback) => {
    const index = args.indexOf(name)
    if (index < 0) return fallback
    if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`Missing value for ${name}`)
    return args[index + 1]
  }
  const appVersion = JSON.parse(await readFile(resolve(appRoot, 'package.json'), 'utf8')).version
  if (typeof appVersion !== 'string' || !/^\d+\.\d+\.\d+(?:[-+][a-zA-Z0-9.-]+)?$/.test(appVersion)) throw new Error('Desktop package version is invalid.')
  const stageRoot = resolve(value('--stage', childPath(appRoot, `toolpacks/stage-${appVersion}`)))
  if (args.includes('--verify')) {
    const inventory = await verifyPack(stageRoot)
    process.stdout.write(`Verified ${inventory.availableToolCount} packaged tools of ${inventory.toolCount}, ${inventory.files.length} files, ${inventory.bytes} bytes.\n`)
    if (inventory.availableToolCount !== inventory.toolCount) process.exitCode = 2
    return
  }
  const sourceRoot = value('--source', process.env.RAINY_NATIVE_TOOLS_SOURCE)
  if (!sourceRoot) throw new Error('Pass --source <CTF tool distribution or libdll directory> or set RAINY_NATIVE_TOOLS_SOURCE.')
  const definition = validateDefinition(JSON.parse(await readFile(value('--manifest', resolve(appRoot, 'toolpacks/native-tools.sources.json')), 'utf8')))
  const report = value('--report', undefined)
  if (args.includes('--plan')) {
    const plan = await planCopies(definition, resolve(sourceRoot))
    const result = { version: 1, toolCount: definition.tools.length, fileCount: plan.files.length, bytes: plan.files.reduce((sum, file) => sum + file.bytes, 0), owners: [...new Set(plan.files.map(file => file.owner))].map(id => ({ id, files: plan.files.filter(file => file.owner === id).length, bytes: plan.files.filter(file => file.owner === id).reduce((sum, file) => sum + file.bytes, 0) })), excluded: plan.excluded }
    if (report) { await mkdir(dirname(resolve(report)), { recursive: true }); await writeFile(resolve(report), JSON.stringify(result, null, 2) + '\n') }
    process.stdout.write(JSON.stringify({ ...result, excluded: result.excluded.length }, null, 2) + '\n')
    return
  }
  const result = await preparePack({ definition, sourceRoot, stageRoot, cacheRoot: resolve(value('--cache', resolve(appRoot, 'toolpacks/cache'))), sevenZip: value('--seven-zip', undefined), offline: args.includes('--offline'), progress: message => process.stdout.write(`${message}\n`) })
  if (report) { await mkdir(dirname(resolve(report)), { recursive: true }); await writeFile(resolve(report), JSON.stringify(result, null, 2) + '\n') }
  process.stdout.write(`Prepared ${result.catalog.tools.length} tools: ${result.fileCount} files, ${result.bytes} bytes, ${result.copied} copied, ${result.reused} reused.\n`)
  if (result.unavailable.length) process.exitCode = 2
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { process.stderr.write(`${error.message}\n`); process.exitCode = 1 })
}
