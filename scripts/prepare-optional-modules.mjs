/** Describe the components that the carrier downloads on demand and split newly built archives into release pieces. */
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import * as tar from 'tar'
import { packReleaseAssets } from './release-assets.mjs'

async function digest(path) {
  const hash = createHash('sha256')
  let bytes = 0
  for await (const chunk of createReadStream(path)) { hash.update(chunk); bytes += chunk.length }
  return { bytes, sha256: hash.digest('hex') }
}

async function unpackedBytes(archive) {
  let total = 0
  await tar.t({ file: archive, strict: true, onReadEntry(entry) {
    // Links in the Linux runtime occupy no space of their own.
    if (entry.type === 'File') total += entry.size
  } })
  return total
}

/**
 * Write the signed-resource descriptor for Strata, PHP and the Linux runtime.
 * Strata keeps the archive already published as a build input; PHP and the Linux runtime receive pieces for this release.
 * @param {{appDirectory:string,releaseVersion:string,pieces:string,output:string,strataArchive:string}} options Locations and identity.
 * @returns {Promise<{modules:object[],uploads:string[]}>} Descriptor entries and the piece files to upload.
 */
export async function prepareOptionalModules(options) {
  const app = resolve(options.appDirectory)
  const inputs = JSON.parse(await readFile(join(app, 'toolpacks/build-inputs.v1.json'), 'utf8'))
  const strata = inputs.files.find(entry => entry.path === 'build-inputs/strata-runtime.tar.gz')
  if (!strata) throw new Error('The Strata build input is not pinned')
  const local = await digest(options.strataArchive)
  if (local.bytes !== strata.bytes || local.sha256 !== strata.sha256) throw new Error('The local Strata archive differs from the pinned build input')
  const baseUrl = `https://github.com/RainyMarks/RainyAgent/releases/download/v${options.releaseVersion}-resources/`
  const scratch = await mkdtemp(join(tmpdir(), 'rainy-modules-'))
  const uploads = []
  try {
    const php = join(app, 'runtime/component-stage/windows-basic/php')
    const names = (await readdir(php)).filter(name => name !== 'dev').sort()
    await tar.c({ cwd: php, file: join(scratch, 'php.tar.gz'), gzip: true, portable: true, mtime: new Date(0) }, names)
    const linuxRuntime = join(app, 'runtime/linux-runtime.tar.gz')
    const linuxMetadata = JSON.parse(await readFile(join(app, 'runtime/linux-runtime.json'), 'utf8'))
    const linux = await digest(linuxRuntime)
    if (linux.bytes !== linuxMetadata.bytes || linux.sha256 !== linuxMetadata.sha256) throw new Error('linux-runtime.json does not describe the staged archive')
    const published = async (id, sourceRoot, file) => {
      const transport = await packReleaseAssets({ sourceRoot, outputDirectory: join(options.pieces, id),
        files: [{ path: file, category: 'offline' }], releaseVersion: options.releaseVersion, baseUrl })
      const entry = transport.files[0]
      uploads.push(...entry.pieces.map(piece => join(options.pieces, id, piece.file)))
      return { file, bytes: entry.bytes, sha256: entry.sha256, baseUrl, pieces: entry.pieces }
    }
    await mkdir(options.pieces, { recursive: true })
    const modules = [
      { id: 'strata', kind: 'directory', file: 'strata-runtime.tar.gz', bytes: strata.bytes, sha256: strata.sha256,
        unpackedBytes: await unpackedBytes(options.strataArchive), baseUrl: strata.baseUrl ?? inputs.baseUrl, pieces: strata.pieces },
      { id: 'php', kind: 'directory', ...await published('php', scratch, 'php.tar.gz'), unpackedBytes: await unpackedBytes(join(scratch, 'php.tar.gz')) },
      { id: 'linux-runtime', kind: 'file', ...await published('linux-runtime', join(app, 'runtime'), 'linux-runtime.tar.gz'),
        unpackedBytes: await unpackedBytes(linuxRuntime) },
    ].map(({ id, kind, file, bytes, sha256, unpackedBytes: unpacked, baseUrl: location, pieces }) =>
      ({ id, kind, file, bytes, sha256, unpackedBytes: unpacked, baseUrl: location, pieces }))
    await writeFile(options.output, JSON.stringify({ version: 1, modules }, null, 2) + '\n')
    return { modules, uploads }
  } finally { await rm(scratch, { recursive: true, force: true }) }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2)
  const values = Object.fromEntries(Array.from({ length: args.length / 2 }, (_, index) => [args[index * 2], args[index * 2 + 1]]))
  for (const name of Object.keys(values)) if (!['--version', '--pieces', '--strata-archive', '--output'].includes(name)) throw new Error(`Unknown option: ${name}`)
  if (args.length % 2 || !values['--version'] || !values['--pieces'] || !values['--strata-archive']) {
    throw new Error('Usage: prepare-optional-modules.mjs --version VERSION --pieces DIR --strata-archive FILE [--output FILE]')
  }
  const app = resolve(import.meta.dirname, '..')
  const result = await prepareOptionalModules({ appDirectory: app, releaseVersion: values['--version'], pieces: resolve(values['--pieces']),
    strataArchive: resolve(values['--strata-archive']), output: resolve(values['--output'] ?? join(app, 'runtime/optional-modules.json')) })
  console.log(JSON.stringify({ modules: result.modules.map(module => `${module.id} ${module.bytes}`), uploads: result.uploads }, null, 2))
}
