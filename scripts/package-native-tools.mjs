/** Build deterministic, inventoried native-tool archives, one per installation unit. */
import { createHash } from 'node:crypto'
import { createReadStream, lstatSync } from 'node:fs'
import { link, lstat, mkdir, mkdtemp, open, readFile, rename, rm, unlink } from 'node:fs/promises'
import { dirname, isAbsolute, parse, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'
import { create as createTar } from 'tar'

const APP_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const APP_VERSION = JSON.parse(await readFile(resolve(APP_ROOT, 'package.json'), 'utf8')).version
const CONTROL_FILES = ['tools/manifest.json', 'tools/verified.json']
const HASH = /^[a-f0-9]{64}$/
const DEVICE = /^(?:con|prn|aux|nul|clock\$|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)/i

const compare = (left, right) => left < right ? -1 : left > right ? 1 : 0
const object = value => value !== null && typeof value === 'object' && !Array.isArray(value)
const byteCount = value => Number.isSafeInteger(value) && value >= 0

function ordinaryPath(value) {
  return typeof value === 'string' && value.length > 0 && !/[\\:\x00-\x1f\x7f<>"|?*]/.test(value)
    && value.split('/').every(part => part !== '' && part !== '.' && part !== '..' && !/[. ]$/.test(part) && !DEVICE.test(part))
}

function requirePath(value, label) {
  if (!ordinaryPath(value)) throw new Error(`${label} must be an ordinary Windows installation-relative path: ${String(value)}`)
  return value
}

function requireStringArray(value, label) {
  if (!Array.isArray(value) || value.some(item => typeof item !== 'string')) throw new Error(`${label} must be an array of paths.`)
  return value.map(item => requirePath(item, label))
}

function recordCase(path, names) {
  const components = path.split('/')
  for (let index = 1; index <= components.length; index++) {
    const part = components.slice(0, index).join('/')
    const key = part.toLowerCase()
    const previous = names.get(key)
    if (previous !== undefined && previous !== part) throw new Error(`Case-insensitive path alias: ${previous} / ${part}`)
    names.set(key, part)
  }
}

function parseInventory(raw) {
  if (!object(raw) || raw.version !== 1 || !byteCount(raw.bytes) || !Array.isArray(raw.files) || raw.files.length === 0
    || raw.algorithm !== undefined && raw.algorithm !== 'sha256') throw new Error('toolpack-files.json must contain a version 1 SHA-256 file inventory and byte total.')
  const paths = new Set()
  const names = new Map()
  let total = 0
  const files = raw.files.map((record) => {
    if (!object(record) || !byteCount(record.bytes) || typeof record.sha256 !== 'string' || !HASH.test(record.sha256)
      || Object.keys(record).some(key => !['path', 'bytes', 'sha256'].includes(key))) throw new Error('Every inventory file requires path, bytes and a lowercase SHA-256 digest.')
    const path = requirePath(record.path, 'Inventory file')
    const key = path.toLowerCase()
    if (paths.has(key)) throw new Error(`Duplicate case-insensitive inventory path: ${path}`)
    paths.add(key)
    recordCase(path, names)
    total += record.bytes
    if (!Number.isSafeInteger(total)) throw new Error('The unpacked byte total exceeds the supported integer range.')
    return { path, bytes: record.bytes, sha256: record.sha256 }
  }).sort((left, right) => compare(left.path, right.path))
  if (total !== raw.bytes) throw new Error('The inventory byte total does not match its files.')
  return { files, unpackedBytes: total, names }
}

function installationUnits(manifest, files, names) {
  if (!object(manifest) || manifest.version !== 1 || !Array.isArray(manifest.tools) || manifest.tools.length === 0) {
    throw new Error('tools/manifest.json must contain a version 1 non-empty tool catalog.')
  }
  const roots = new Map()
  const ids = new Set()
  const preserved = new Set()
  for (const tool of manifest.tools) {
    if (!object(tool) || typeof tool.id !== 'string' || !/^[a-z0-9][a-z0-9-]*$/.test(tool.id) || ids.has(tool.id)) {
      throw new Error('Tool identities must be unique lowercase names.')
    }
    ids.add(tool.id)
    const ownRoot = `tools/${tool.id}`
    const toolRoots = requireStringArray(tool.roots, `Roots for ${tool.id}`)
    if (!toolRoots.includes(ownRoot)) throw new Error(`Tool ${tool.id} must declare its own directory.`)
    const localRoots = new Set()
    for (const path of toolRoots) {
      if (path !== ownRoot && !/^runtime\/windows\/[^/]+$/.test(path)) throw new Error(`Unsupported installation directory: ${path}`)
      if (localRoots.has(path.toLowerCase())) throw new Error(`Duplicate tool root: ${path}`)
      localRoots.add(path.toLowerCase())
      recordCase(path, names)
      if (!roots.has(path)) roots.set(path, { path, kind: 'directory', preserve: [] })
    }
    const preserve = tool.preserve === undefined ? [] : requireStringArray(tool.preserve, `Preserved paths for ${tool.id}`)
    for (const path of preserve) {
      recordCase(path, names)
      const root = toolRoots.find(candidate => path.startsWith(candidate + '/'))
      if (root === undefined) throw new Error(`Preserved path is outside the tool directories: ${path}`)
      if (preserved.has(path.toLowerCase())) throw new Error(`Duplicate preserved path: ${path}`)
      preserved.add(path.toLowerCase())
      roots.get(root).preserve.push(path)
    }
  }
  const directories = [...roots.values()].sort((left, right) => compare(left.path, right.path))
  for (const unit of directories) {
    unit.preserve.sort(compare)
    for (let index = 0; index < unit.preserve.length; index++) {
      const path = unit.preserve[index].toLowerCase()
      if (unit.preserve.some((other, otherIndex) => otherIndex !== index && other.toLowerCase().startsWith(path + '/'))) {
        throw new Error(`Overlapping preserved paths: ${unit.preserve[index]}`)
      }
    }
    if (!files.some(file => file.path.startsWith(unit.path + '/'))) throw new Error(`Installation directory has no inventoried files: ${unit.path}`)
  }
  if (!files.some(file => file.path === CONTROL_FILES[0])) throw new Error('The file inventory must include tools/manifest.json.')
  const controls = CONTROL_FILES.filter(path => files.some(file => file.path === path)).map(path => ({ path, kind: 'file', preserve: [] }))
  const units = [...directories, ...controls]
  for (const file of files) {
    if (!units.some(unit => unit.kind === 'file' ? file.path === unit.path : file.path.startsWith(unit.path + '/'))) {
      throw new Error(`Inventory file is outside the installation directories: ${file.path}`)
    }
  }
  return units
}

function absoluteParts(path) {
  const absolute = resolve(path)
  const root = parse(absolute).root
  const names = absolute.slice(root.length).split(sep).filter(Boolean)
  const paths = [root]
  for (const name of names) paths.push(resolve(paths[paths.length - 1], name))
  return paths
}

async function realDirectory(path, create = false) {
  for (const part of absoluteParts(path)) {
    let stat
    try { stat = await lstat(part) }
    catch (error) {
      if (!create || error.code !== 'ENOENT') throw error
      try { await mkdir(part) }
      catch (creationError) { if (creationError.code !== 'EEXIST') throw creationError }
      stat = await lstat(part)
    }
    if (stat.isSymbolicLink() || !stat.isDirectory()) throw new Error(`Directory must not be a symlink or junction: ${part}`)
  }
}

async function realFile(path) {
  await realDirectory(dirname(path))
  const stat = await lstat(path)
  if (stat.isSymbolicLink() || !stat.isFile() || stat.nlink !== 1) throw new Error(`Source must be an independent regular file, without links: ${path}`)
  return stat
}

async function readJson(path) {
  await realFile(path)
  try { return JSON.parse(await readFile(path, 'utf8')) }
  catch (error) { throw new Error(`Cannot read JSON: ${path}`, { cause: error }) }
}

async function fingerprint(path) {
  const hash = createHash('sha256')
  let bytes = 0
  for await (const chunk of createReadStream(path)) { hash.update(chunk); bytes += chunk.length }
  return { bytes, sha256: hash.digest('hex') }
}

async function verifySources(stage, files) {
  const stats = new Map()
  for (const file of files) {
    const path = resolve(stage, ...file.path.split('/'))
    const before = await realFile(path)
    if (before.size !== file.bytes) throw new Error(`Source byte count does not match the inventory: ${file.path}`)
    const actual = await fingerprint(path)
    const after = await realFile(path)
    if (actual.bytes !== file.bytes || actual.sha256 !== file.sha256) throw new Error(`Source SHA-256 does not match the inventory: ${file.path}`)
    if (before.mtimeMs !== after.mtimeMs || before.ctimeMs !== after.ctimeMs || before.ino !== after.ino || before.dev !== after.dev) {
      throw new Error(`Source changed while it was verified: ${file.path}`)
    }
    stats.set(file.path, after)
  }
  return stats
}

function checkArchiveSource(stage, path, stat, expected) {
  if (expected === undefined || !stat.isFile() || stat.nlink !== 1 || stat.size !== expected.size
    || stat.ino !== expected.ino || stat.dev !== expected.dev || stat.mtimeMs !== expected.mtimeMs || stat.ctimeMs !== expected.ctimeMs) {
    throw new Error(`Source changed before archiving: ${path}`)
  }
  for (const part of absoluteParts(resolve(stage, ...path.split('/')))) {
    const current = lstatSync(part)
    if (current.isSymbolicLink()) throw new Error(`Archive source traverses a link: ${path}`)
  }
}

async function archiveUnit(stage, temporary, file, paths, stats) {
  const hash = createHash('sha256')
  let bytes = 0
  const handle = await open(resolve(temporary, file), 'wx', 0o600)
  try {
    let archive
    archive = createTar({ cwd: stage, gzip: true, portable: true, mtime: new Date(0), strict: true,
      noDirRecurse: true, follow: false, filter: (path, stat) => {
        try { checkArchiveSource(stage, path, stat, stats.get(path)); return true }
        catch (error) { archive.destroy(error); return false }
      } }, paths)
    for await (const chunk of archive) {
      hash.update(chunk)
      bytes += chunk.length
      let written = 0
      while (written < chunk.length) {
        const result = await handle.write(chunk, written, chunk.length - written)
        if (result.bytesWritten === 0) throw new Error(`Archive write made no progress: ${file}`)
        written += result.bytesWritten
      }
    }
    await handle.sync()
  } finally { await handle.close() }
  return { bytes, sha256: hash.digest('hex') }
}

async function publishArchive(temporary, output, archive) {
  const source = resolve(temporary, archive.file)
  const destination = resolve(output, archive.file)
  try { await link(source, destination) }
  catch (error) {
    if (error.code !== 'EEXIST') throw error
    const stat = await lstat(destination)
    if (stat.isSymbolicLink() || !stat.isFile()) throw new Error(`Archive output is not a regular file: ${archive.file}`)
    const existing = await fingerprint(destination)
    if (existing.bytes !== archive.bytes || existing.sha256 !== archive.sha256) throw new Error(`Existing unit archive has different bytes: ${archive.file}`)
  }
  await unlink(source)
}

async function writeDurable(path, bytes) {
  const handle = await open(path, 'wx', 0o600)
  try { await handle.writeFile(bytes); await handle.sync() }
  finally { await handle.close() }
}

async function cleanupTemporary(output, temporary) {
  const child = relative(output, temporary)
  if (child === '' || child.startsWith('..') || isAbsolute(child) || child.includes(sep)) throw new Error('Temporary archive cleanup escaped its output directory.')
  const stat = await lstat(temporary)
  if (stat.isSymbolicLink()) await unlink(temporary)
  else if (stat.isDirectory()) await rm(temporary, { recursive: true, force: true })
  else throw new Error('Temporary archive path is no longer a directory.')
}

/**
 * Name a unit archive by its file inventory, so a later pack reuses the archive of an unchanged tool.
 * @param {{path:string,kind:string}} unit Installation unit.
 * @param {Array<{path:string,bytes:number,sha256:string}>} files The unit's inventoried files.
 * @returns {string} Archive file name.
 */
export function unitArchiveName(unit, files) {
  return `rainy-unit-${createHash('sha256').update(JSON.stringify({ unit: unit.path, kind: unit.kind, files })).digest('hex').slice(0, 20)}.tar.gz`
}

/**
 * Verify source bytes and publish one archive per installation unit before atomically replacing their metadata.
 * @param {{stage?: string, output?: string, previous?: string}} options Source stage, output directory and earlier
 *   version 2 metadata whose archive records are reused for units with identical files.
 * @returns {Promise<object>} Published version 2 metadata; archives of earlier packs remain available.
 */
export async function packageNativeTools({ stage = resolve(APP_ROOT, `toolpacks/stage-${APP_VERSION}`),
  output = resolve(APP_ROOT, `release/offline-${APP_VERSION}`), previous } = {}) {
  if (typeof stage !== 'string' || typeof output !== 'string' || previous !== undefined && typeof previous !== 'string') {
    throw new Error('Stage, output and previous metadata must be paths.')
  }
  stage = resolve(stage)
  output = resolve(output)
  const outputWithinStage = relative(stage, output)
  if (outputWithinStage === '' || !outputWithinStage.startsWith('..') && !isAbsolute(outputWithinStage)) throw new Error('Archive output must be outside the source stage.')
  await realDirectory(stage)
  const { files, unpackedBytes, names } = parseInventory(await readJson(resolve(stage, 'toolpack-files.json')))
  const units = installationUnits(await readJson(resolve(stage, 'tools/manifest.json')), files, names)
  const reusable = new Map()
  if (previous !== undefined) {
    const earlier = await readJson(resolve(previous))
    if (!object(earlier) || earlier.version !== 2 || !Array.isArray(earlier.archives)) throw new Error('Previous metadata must be a version 2 tool pack.')
    for (const archive of earlier.archives) {
      if (!object(archive) || typeof archive.file !== 'string' || !/^rainy-unit-[a-f0-9]{20}\.tar\.gz$/.test(archive.file)
        || !Number.isSafeInteger(archive.bytes) || archive.bytes <= 0 || typeof archive.sha256 !== 'string' || !HASH.test(archive.sha256)) {
        throw new Error('Previous metadata contains an invalid unit archive.')
      }
      reusable.set(archive.file, { bytes: archive.bytes, sha256: archive.sha256 })
    }
  }
  const stats = await verifySources(stage, files)
  const id = createHash('sha256').update(JSON.stringify({ files, units })).digest('hex')
  await realDirectory(output, true)
  const temporary = await mkdtemp(resolve(output, '.native-toolpack-'))
  try {
    const archives = []
    const built = []
    for (const unit of units) {
      const unitFiles = files.filter(file => unit.kind === 'file' ? file.path === unit.path : file.path.startsWith(unit.path + '/'))
      const file = unitArchiveName(unit, unitFiles)
      const earlier = reusable.get(file)
      if (earlier !== undefined) { archives.push({ unit: unit.path, file, ...earlier }); continue }
      const archive = { unit: unit.path, file, ...await archiveUnit(stage, temporary, file, unitFiles.map(entry => entry.path), stats) }
      archives.push(archive)
      built.push(archive)
    }
    const metadata = { version: 2, id, format: 'tar.gz', unpackedBytes, files, units, archives }
    await writeDurable(resolve(temporary, 'native-tools-metadata.json'), JSON.stringify(metadata, null, 2) + '\n')
    await realDirectory(output)
    for (const archive of built) await publishArchive(temporary, output, archive)
    const metadataPath = resolve(output, 'native-tools-metadata.json')
    try {
      const existing = await lstat(metadataPath)
      if (existing.isSymbolicLink() || !existing.isFile()) throw new Error('Existing tool-pack metadata is not a regular file.')
    } catch (error) { if (error.code !== 'ENOENT') throw error }
    await rename(resolve(temporary, 'native-tools-metadata.json'), metadataPath)
    return metadata
  } finally {
    await cleanupTemporary(output, temporary)
  }
}

function argumentsFrom(argv) {
  const result = {}
  const keys = new Map([['--stage', 'stage'], ['--output', 'output'], ['--previous', 'previous']])
  for (let index = 0; index < argv.length; index += 2) {
    const key = keys.get(argv[index])
    const value = argv[index + 1]
    if (key === undefined || value === undefined || value.startsWith('--') || Object.hasOwn(result, key)) {
      throw new Error('Usage: package-native-tools.mjs [--stage <directory>] [--output <directory>] [--previous <metadata.json>]')
    }
    result[key] = value
  }
  return result
}

async function main() {
  const metadata = await packageNativeTools(argumentsFrom(process.argv.slice(2)))
  console.log(JSON.stringify({ id: metadata.id, files: metadata.files.length, unpackedBytes: metadata.unpackedBytes,
    archives: metadata.archives.length, compressedBytes: metadata.archives.reduce((sum, archive) => sum + archive.bytes, 0) }))
}

if (process.argv[1] !== undefined && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch(error => { console.error(error.message); process.exitCode = 1 })
}
