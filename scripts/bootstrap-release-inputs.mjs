/** Restore pinned release inputs for the existing Windows and WSL packaging scripts. */
import { createHash, randomUUID } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { copyFile, link, mkdir, open, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { basename, dirname, join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { isDeepStrictEqual } from 'node:util'
import * as tar from 'tar'
import { releaseDigest, releasePath, releaseTarget, validateReleaseManifest } from './release-assets.mjs'

async function matches(path, expected) {
  try {
    const actual = await releaseDigest(path)
    if (actual.bytes !== expected.bytes || actual.sha256 !== expected.sha256) throw new Error(`Release checksum differs: ${path}`)
    return true
  } catch (error) {
    if (error.code === 'ENOENT') return false
    throw error
  }
}

async function writeVerified(stream, target, expected) {
  const temporary = target + `.${randomUUID()}.pending`
  const handle = await open(temporary, 'wx')
  const hash = createHash('sha256')
  let bytes = 0
  try {
    for await (const value of stream) {
      const chunk = Buffer.from(value)
      bytes += chunk.length
      if (bytes > expected.bytes) throw new Error(`Release file exceeds declared size: ${target}`)
      hash.update(chunk)
      let offset = 0
      while (offset < chunk.length) {
        const result = await handle.write(chunk, offset, chunk.length - offset)
        if (!result.bytesWritten) throw new Error(`Release input write stalled: ${target}`)
        offset += result.bytesWritten
      }
    }
    if (bytes !== expected.bytes || hash.digest('hex') !== expected.sha256) throw new Error(`Release checksum differs: ${target}`)
    await handle.close()
    await link(temporary, target)
  } finally {
    await handle.close()
    await rm(temporary, { force: true })
  }
}

/**
 * Verify prepared files or download and reassemble pinned transport pieces with bounded memory.
 * @param {{manifest:object,outputDirectory:string,inputsDirectory?:string,baseUrl?:string}} options A validated source manifest and optional local inputs.
 * @returns {Promise<void>} Resolves after all offline and build inputs match their original hashes.
 */
export async function restoreReleaseInputs(options) {
  const manifest = validateReleaseManifest(options.manifest)
  if (options.inputsDirectory && options.baseUrl) throw new Error('Choose --inputs-dir or --base-url')
  const baseUrl = options.baseUrl ?? manifest.baseUrl
  if (baseUrl !== manifest.baseUrl) throw new Error('Release URL differs from the source-pinned manifest')
  await mkdir(options.outputDirectory, { recursive: true })
  for (const entry of manifest.files.filter(item => item.category !== 'installer')) {
    const target = await releaseTarget(options.outputDirectory, entry.path)
    await mkdir(dirname(target), { recursive: true })
    if (await matches(target, entry)) continue
    if (options.inputsDirectory) {
      const source = await releaseTarget(options.inputsDirectory, entry.path)
      if (!(await matches(source, entry))) throw new Error(`Missing release input: ${entry.path}`)
      await writeVerified(createReadStream(source), target, entry)
    } else {
      async function* pieces() {
        for (const piece of entry.pieces) {
          const response = await fetch(new URL(piece.file, entry.baseUrl ?? baseUrl))
          if (!response.ok || !response.body) throw new Error(`Release download failed: ${piece.file} (${response.status})`)
          const hash = createHash('sha256')
          let bytes = 0
          for await (const chunk of response.body) {
            bytes += chunk.length
            if (bytes > piece.bytes) throw new Error(`Release piece exceeds declared size: ${piece.file}`)
            hash.update(chunk)
            yield chunk
          }
          if (bytes !== piece.bytes || hash.digest('hex') !== piece.sha256) throw new Error(`Release piece checksum differs: ${piece.file}`)
        }
      }
      await writeVerified(pieces(), target, entry)
    }
  }
}

/**
 * Extract only regular files and directories; existing different destination files are preserved.
 * @param {string} archive Verified gzip tar archive.
 * @param {string} destination Builder-owned generated directory.
 * @returns {Promise<void>} Resolves after safe archive members have been copied.
 */
export async function extractBuildInput(archive, destination) {
  await releaseTarget(dirname(destination), basename(destination))
  const names = new Map()
  let invalid
  await tar.t({ file: archive, strict: true, onReadEntry(entry) {
    try {
      const path = releasePath(entry.path.replace(/\/$/u, ''))
      const key = path.toLowerCase()
      if (!['File', 'Directory'].includes(entry.type) || names.has(key)) throw new Error(`Unsafe or duplicate archive member: ${entry.path}`)
      names.set(key, entry.type)
    } catch (error) { invalid ??= error }
  } })
  if (invalid) throw invalid
  for (const name of names.keys()) {
    const parents = name.split('/')
    while (parents.pop() !== undefined && parents.length) {
      const parent = parents.join('/')
      if (names.get(parent) === 'File') throw new Error(`Archive file/directory collision: ${parent}`)
    }
  }
  await mkdir(destination, { recursive: true })
  const staging = join(destination, `.release-input-${randomUUID()}`)
  await mkdir(staging)
  try {
    await tar.x({ file: archive, cwd: staging, strict: true, preservePaths: false, noChmod: false })
    async function merge(relative = '') {
      for (const entry of await readdir(join(staging, relative), { withFileTypes: true })) {
        const path = relative ? `${relative}/${entry.name}` : entry.name
        const source = join(staging, path)
        const target = await releaseTarget(destination, path)
        if (entry.isDirectory()) { await mkdir(target, { recursive: true }); await merge(path) }
        else if (entry.isFile()) {
          const expected = await releaseDigest(source)
          if (!(await matches(target, expected))) await copyFile(source, target, 1)
        } else throw new Error(`Extracted release input is not a regular file: ${path}`)
      }
    }
    await merge()
  } finally { await rm(staging, { recursive: true, force: true, maxRetries: 5 }) }
}

/**
 * Create a component catalog or retain an equivalent existing catalog regardless of component or JSON key order.
 * @param {string} path Builder-owned catalog destination.
 * @param {object[]} components Descriptors authenticated against the pinned release inputs.
 * @returns {Promise<void>} Resolves after creating the catalog or verifying every existing descriptor and the catalog version.
 */
export async function writeReleaseComponentCatalog(path, components) {
  const catalog = { version: 1, components }
  try { await writeFile(path, JSON.stringify(catalog, null, 2) + '\n', { flag: 'wx' }); return }
  catch (error) { if (error.code !== 'EEXIST') throw error }
  const existing = JSON.parse(await readFile(path, 'utf8'))
  function ordered(value) {
    if (!Array.isArray(value?.components) || value.components.some(component => typeof component?.id !== 'string')) return value
    return { ...value, components: [...value.components].sort((left, right) => left.id.localeCompare(right.id)) }
  }
  if (!isDeepStrictEqual(ordered(existing), ordered(catalog))) throw new Error(`Existing component catalog differs: ${path}`)
}

/**
 * Populate ignored locations consumed by package.ps1 from hash-pinned release files.
 * @param {{appDirectory:string,manifest:object,inputsDirectory?:string,baseUrl?:string}} options Checkout and authenticated input inventory.
 * @returns {Promise<string>} Prepared offline directory for -ReuseNativeToolsRelease and -ComponentSource.
 */
export async function bootstrapReleaseInputs(options) {
  const manifest = validateReleaseManifest(options.manifest)
  const app = resolve(options.appDirectory)
  const packageInfo = JSON.parse(await readFile(join(app, 'package.json'), 'utf8'))
  if (packageInfo.version !== manifest.releaseVersion) throw new Error('Release input version differs from the desktop package')
  const required = ['build-inputs/ide-resources.tar.gz', 'build-inputs/strata-runtime.tar.gz', 'environment-components/windows-basic.tar.gz', 'native-tools-metadata.json',
    'environment/media-verification.json']
  for (const path of required) if (!manifest.files.some(entry => entry.path === path && entry.category !== 'installer')) throw new Error(`Required build input is absent: ${path}`)
  const offline = join(app, `release/offline-${manifest.releaseVersion}`)
  await restoreReleaseInputs({ ...options, outputDirectory: offline })
  await extractBuildInput(join(offline, 'build-inputs/ide-resources.tar.gz'), join(app, 'resources/ide'))
  await extractBuildInput(join(offline, 'build-inputs/strata-runtime.tar.gz'), join(app, 'resources/strata-runtime'))
  await extractBuildInput(join(offline, 'environment-components/windows-basic.tar.gz'), join(app, 'runtime/component-stage/windows-basic'))
  const catalog = []
  for (const entry of manifest.files.filter(item => /^environment-components\/[^/]+\.json$/u.test(item.path))) {
    const descriptor = JSON.parse(await readFile(join(offline, entry.path), 'utf8'))
    const archive = manifest.files.find(item => item.path === `environment-components/${descriptor.file}`)
    if (!archive || descriptor.bytes !== archive.bytes || descriptor.sha256 !== archive.sha256) throw new Error(`Component descriptor differs from release inputs: ${entry.path}`)
    catalog.push(descriptor)
  }
  for (const entry of manifest.files.filter(item => item.path.startsWith('environment/'))) {
    const target = await releaseTarget(join(app, 'runtime'), entry.path)
    await mkdir(dirname(target), { recursive: true })
    if (!(await matches(target, entry))) await copyFile(join(offline, entry.path), target, 1)
  }
  const catalogPath = await releaseTarget(join(app, 'runtime'), 'environment-component-catalog.json')
  await writeReleaseComponentCatalog(catalogPath, catalog)
  return offline
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2)
  if (args.length % 2) throw new Error('Usage: bootstrap-release-inputs.mjs --manifest FILE [--inputs-dir DIR | --base-url PINNED_URL]')
  const values = Object.fromEntries(Array.from({ length: args.length / 2 }, (_, index) => [args[index * 2], args[index * 2 + 1]]))
  for (const name of Object.keys(values)) if (!['--manifest', '--inputs-dir', '--base-url'].includes(name)) throw new Error(`Unknown option: ${name}`)
  if (!values['--manifest']) throw new Error('--manifest must name the source-pinned release manifest')
  const offline = await bootstrapReleaseInputs({ appDirectory: resolve(import.meta.dirname, '..'),
    manifest: JSON.parse(await readFile(values['--manifest'], 'utf8')), inputsDirectory: values['--inputs-dir'], baseUrl: values['--base-url'] })
  console.log(`Verified build inputs: ${offline}`)
}
