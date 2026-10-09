/** Publish flat, checksum-verified transport pieces without changing the enclosed release files. */
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { copyFile, lstat, mkdir, mkdtemp, open, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { basename, dirname, join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const PART_BYTES = 1024 ** 3
const categories = new Set(['installer', 'offline', 'build-input'])
const hashPattern = /^[a-f0-9]{64}$/u

/**
 * Validate a portable release-relative file name, including Windows aliases.
 * @param {string} name Untrusted manifest path.
 * @returns {string} The validated path with forward slashes.
 */
export function releasePath(name) {
  if (typeof name !== 'string' || !name || name.length > 220 || name.split('/').some(part =>
    !part || part === '.' || part === '..' || /[\\:<>"|?*\x00-\x1f]/u.test(part) || /[. ]$/u.test(part)
    || /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/iu.test(part))) {
    throw new Error(`Unsafe release path: ${String(name)}`)
  }
  return name
}

/**
 * Resolve a manifest path and reject existing links at every owned path segment.
 * @param {string} root Explicit source or destination directory.
 * @param {string} name Release-relative path.
 * @returns {Promise<string>} Absolute path beneath root.
 */
export async function releaseTarget(root, name) {
  releasePath(name)
  let current = resolve(root)
  for (const part of ['', ...name.split('/')]) {
    if (part) current = join(current, part)
    try {
      const info = await lstat(current)
      if (info.isSymbolicLink()) throw new Error(`Release path contains a link: ${current}`)
    } catch (error) {
      if (error.code !== 'ENOENT') throw error
    }
  }
  return current
}

/**
 * Hash a regular file with bounded memory.
 * @param {string} path File to read.
 * @returns {Promise<{bytes:number,sha256:string}>} Observed size and SHA-256.
 */
export async function releaseDigest(path) {
  const info = await lstat(path)
  if (!info.isFile() || info.isSymbolicLink()) throw new Error(`Release input is not a regular file: ${path}`)
  const hash = createHash('sha256')
  let bytes = 0
  for await (const chunk of createReadStream(path)) { hash.update(chunk); bytes += chunk.length }
  return { bytes, sha256: hash.digest('hex') }
}

/**
 * Validate a JSON transport manifest before reading files or creating output.
 * @param {unknown} input Parsed JSON.
 * @returns {object} Validated manifest.
 */
export function validateReleaseManifest(input) {
  if (!input || input.version !== 1 || typeof input.releaseVersion !== 'string'
    || !/^\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?$/u.test(input.releaseVersion)
    || !Number.isSafeInteger(input.partBytes) || input.partBytes < 1 || input.partBytes > PART_BYTES
    || !Array.isArray(input.files) || input.files.length === 0) throw new Error('Invalid release manifest')
  function pinnedUrl(value) {
    const url = new URL(value)
    if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash || !url.pathname.endsWith('/')
      || /\/(?:latest|latest\/download)\//u.test(url.pathname)) throw new Error('Release base URL must be a pinned HTTPS directory')
  }
  pinnedUrl(input.baseUrl)
  const paths = new Set()
  const pieces = new Set(['release-assets.json'])
  for (const entry of input.files) {
    const key = releasePath(entry.path).toLowerCase()
    if (paths.has(key) || [...paths].some(path => path.startsWith(key + '/') || key.startsWith(path + '/'))) throw new Error(`Release path collision: ${entry.path}`)
    paths.add(key)
    if (!categories.has(entry.category) || !Number.isSafeInteger(entry.bytes) || entry.bytes < 0 || !hashPattern.test(entry.sha256)
      || !Array.isArray(entry.pieces) || entry.pieces.length === 0) throw new Error(`Invalid release file: ${entry.path}`)
    if (entry.baseUrl !== undefined) pinnedUrl(entry.baseUrl)
    let size = 0
    for (const piece of entry.pieces) {
      const name = releasePath(piece.file)
      if (name.includes('/') || pieces.has(name.toLowerCase()) || !Number.isSafeInteger(piece.bytes) || piece.bytes < 0
        || piece.bytes >= 2 * 1024 ** 3 || !hashPattern.test(piece.sha256)
        || entry.category !== 'installer' && piece.bytes > input.partBytes) throw new Error(`Invalid release piece: ${piece.file}`)
      pieces.add(name.toLowerCase())
      size += piece.bytes
    }
    if (!Number.isSafeInteger(size) || size !== entry.bytes) throw new Error(`Release piece sizes differ: ${entry.path}`)
    if (entry.category === 'installer' && (entry.pieces.length !== 1 || entry.pieces[0].file !== basename(entry.path)
      || entry.pieces[0].sha256 !== entry.sha256)) throw new Error('The installer must remain one raw asset')
  }
  return input
}

/**
 * Split an explicit file allowlist into a new output directory; incomplete output is never a manifest.
 * @param {{sourceRoot:string,outputDirectory:string,files:Array<{path:string,category:string}>,releaseVersion:string,baseUrl:string,partBytes?:number}} options File selection and publication identity.
 * @returns {Promise<object>} Completed transport manifest.
 */
export async function packReleaseAssets(options) {
  const limit = options.partBytes ?? PART_BYTES
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > PART_BYTES || !Array.isArray(options.files) || options.files.length === 0) throw new Error('Invalid release packing inputs')
  const files = [...options.files].sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0)
  const manifest = { version: 1, releaseVersion: options.releaseVersion, baseUrl: options.baseUrl, partBytes: limit,
    files: files.map(entry => ({ ...entry, bytes: 0, sha256: '0'.repeat(64), pieces: [{ file: entry.category === 'installer' ? basename(entry.path) : `rainy-${createHash('sha256').update(entry.path).digest('hex').slice(0, 20)}.001`, bytes: 0, sha256: '0'.repeat(64) }] })) }
  validateReleaseManifest(manifest)
  for (const entry of files) {
    const source = await releaseTarget(options.sourceRoot, entry.path)
    const info = await lstat(source)
    if (!info.isFile() || entry.category === 'installer' && info.size >= 2 * 1024 ** 3) throw new Error(`Invalid release source: ${entry.path}`)
  }
  const output = resolve(options.outputDirectory)
  await mkdir(dirname(output), { recursive: true })
  await mkdir(output)
  for (const entry of manifest.files) {
    const source = await releaseTarget(options.sourceRoot, entry.path)
    const original = createHash('sha256')
    const prefix = entry.pieces[0].file.replace(/\.001$/u, '')
    entry.pieces = []
    let handle
    let piece
    let hash
    async function nextPiece() {
      const file = entry.category === 'installer' ? basename(entry.path) : `${prefix}.${String(entry.pieces.length + 1).padStart(3, '0')}`
      handle = await open(join(output, file), 'wx')
      piece = { file, bytes: 0, sha256: '' }
      hash = createHash('sha256')
    }
    async function finishPiece() {
      await handle.close()
      handle = undefined
      piece.sha256 = hash.digest('hex')
      entry.pieces.push(piece)
    }
    try {
      await nextPiece()
      for await (const chunk of createReadStream(source, { highWaterMark: 1024 * 1024 })) {
        original.update(chunk)
        entry.bytes += chunk.length
        for (let offset = 0; offset < chunk.length;) {
          if (!handle) await nextPiece()
          const bytes = chunk.subarray(offset, offset + Math.min(chunk.length - offset, entry.category === 'installer' ? chunk.length : limit - piece.bytes))
          let written = 0
          while (written < bytes.length) {
            const result = await handle.write(bytes, written, bytes.length - written)
            if (!result.bytesWritten) throw new Error(`Release piece write stalled: ${piece.file}`)
            written += result.bytesWritten
          }
          hash.update(bytes)
          piece.bytes += bytes.length
          offset += bytes.length
          if (entry.category !== 'installer' && piece.bytes === limit) await finishPiece()
        }
      }
      if (handle) await finishPiece()
      entry.sha256 = original.digest('hex')
    } finally { if (handle) await handle.close() }
  }
  validateReleaseManifest(manifest)
  await writeFile(join(output, 'release-assets.json'), JSON.stringify(manifest, null, 2) + '\n', { flag: 'wx' })
  return manifest
}

/**
 * Append raw installer and updater assets without rereading unchanged offline pieces.
 * @param {{manifest:object,sourceRoot:string,outputDirectory:string,files:Array<{path:string,category:string}>}} options Original frozen manifest and new raw assets.
 * @returns {Promise<object>} Distribution manifest including the new raw assets.
 */
export async function extendReleaseAssets(options) {
  const baseline = validateReleaseManifest(options.manifest)
  if (!Array.isArray(options.files) || !options.files.length || options.files.some(entry => entry.category !== 'installer')) throw new Error('Release extension accepts only raw installer or updater assets')
  const output = resolve(options.outputDirectory)
  const originalPath = await releaseTarget(output, 'release-assets.json')
  if (JSON.stringify(JSON.parse(await readFile(originalPath, 'utf8'))) !== JSON.stringify(baseline)) throw new Error('Transport output does not match the frozen input manifest')
  const provisional = options.files.map(entry => ({ ...entry, bytes: 0, sha256: '0'.repeat(64), pieces: [{ file: basename(entry.path), bytes: 0, sha256: '0'.repeat(64) }] }))
  validateReleaseManifest({ ...baseline, files: [...baseline.files, ...provisional] })
  const temporary = await mkdtemp(join(dirname(output), 'rainy-release-raw-'))
  try {
    const raw = await packReleaseAssets({ sourceRoot: options.sourceRoot, outputDirectory: join(temporary, 'assets'), files: options.files,
      releaseVersion: baseline.releaseVersion, baseUrl: baseline.baseUrl, partBytes: baseline.partBytes })
    const manifest = validateReleaseManifest({ ...baseline, files: [...baseline.files, ...raw.files].sort((a, b) => a.path < b.path ? -1 : a.path > b.path ? 1 : 0) })
    for (const entry of raw.files) {
      const target = await releaseTarget(output, entry.pieces[0].file)
      await copyFile(join(temporary, 'assets', entry.pieces[0].file), target, 1)
    }
    const pending = join(temporary, 'release-assets.json')
    await writeFile(pending, JSON.stringify(manifest, null, 2) + '\n')
    if (JSON.stringify(JSON.parse(await readFile(originalPath, 'utf8'))) !== JSON.stringify(baseline)) throw new Error('Transport manifest changed during extension')
    await rename(pending, originalPath)
    return manifest
  } finally { await rm(temporary, { recursive: true, force: true, maxRetries: 5 }) }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2)
  const command = args.shift()
  if (!['pack', 'extend'].includes(command) || args.length % 2) throw new Error('Usage: release-assets.mjs pack|extend --source DIR --output DIR --files ALLOWLIST.json [--version VERSION --base-url URL | --manifest FROZEN_INPUTS.json]')
  const values = Object.fromEntries(Array.from({ length: args.length / 2 }, (_, index) => [args[index * 2], args[index * 2 + 1]]))
  for (const name of Object.keys(values)) if (!['--source', '--output', '--files', '--version', '--base-url', '--manifest'].includes(name)) throw new Error(`Unknown option: ${name}`)
  const options = { sourceRoot: values['--source'], outputDirectory: values['--output'], files: JSON.parse(await readFile(values['--files'], 'utf8')) }
  const manifest = command === 'extend' ? await extendReleaseAssets({ ...options, manifest: JSON.parse(await readFile(values['--manifest'], 'utf8')) })
    : await packReleaseAssets({ ...options, releaseVersion: values['--version'], baseUrl: values['--base-url'] })
  console.log(JSON.stringify({ files: manifest.files.length, pieces: manifest.files.reduce((count, entry) => count + entry.pieces.length, 0) }))
}
