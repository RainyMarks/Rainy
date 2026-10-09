/** Publish a per-tool pack: split new unit archives into release pieces, record where every archive lives, and sign the channel. */
import { createHash } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { packReleaseAssets } from './release-assets.mjs'
import { signToolChannel, writeToolChannel } from './sign-tool-channel.mjs'

/**
 * Build the download source and signed channel for version 2 metadata.
 * Archives already listed in the previous source keep their release location; only the others need new pieces.
 * @param {{metadataPath:string,archives:string,catalogPath:string,releaseVersion:string,revision:number,pieces:string,
 *   sourceOutput:string,channelOutput:string,previousSource?:string,privateKeyPath?:string,publicKeysPath?:string,
 *   channelPaths?:{published?:string,legacy?:string}}} options Build inputs; writing the published channel also refreshes its 1.x copy.
 * @returns {Promise<{source:object,uploads:string[]}>} Download source and the piece files to upload to the resources release.
 */
export async function buildToolChannel(options) {
  const metadata = JSON.parse(await readFile(options.metadataPath, 'utf8'))
  if (metadata.version !== 2 || !Array.isArray(metadata.archives)) throw new Error('Expected version 2 tool-pack metadata')
  const previous = options.previousSource === undefined ? undefined : JSON.parse(await readFile(options.previousSource, 'utf8'))
  if (previous !== undefined && (previous.version !== 2 || !Array.isArray(previous.archives))) throw new Error('The previous source must use format 2')
  const known = new Map((previous?.archives ?? []).map(archive => [archive.file, archive]))
  const baseUrl = `https://github.com/RainyMarks/RainyAgent/releases/download/v${options.releaseVersion}-resources/`
  const fresh = metadata.archives.filter(archive => known.get(archive.file)?.sha256 !== archive.sha256)
  const uploads = []
  const published = new Map()
  if (fresh.length > 0) {
    await mkdir(resolve(options.pieces, '..'), { recursive: true })
    const transport = await packReleaseAssets({ sourceRoot: options.archives, outputDirectory: options.pieces,
      files: fresh.map(archive => ({ path: archive.file, category: 'offline' })), releaseVersion: options.releaseVersion, baseUrl })
    for (const entry of transport.files) {
      published.set(entry.path, { file: entry.path, bytes: entry.bytes, sha256: entry.sha256, baseUrl, pieces: entry.pieces })
      uploads.push(...entry.pieces.map(piece => join(options.pieces, piece.file)))
    }
  }
  const archives = metadata.archives.map((archive) => {
    const entry = published.get(archive.file) ?? known.get(archive.file)
    if (!entry || entry.bytes !== archive.bytes || entry.sha256 !== archive.sha256) throw new Error(`Archive bytes differ from metadata: ${archive.file}`)
    return { file: entry.file, bytes: entry.bytes, sha256: entry.sha256, baseUrl: entry.baseUrl, pieces: entry.pieces }
  })
  const source = { version: 2, packId: metadata.id, archives }
  await writeFile(options.sourceOutput, JSON.stringify(source, null, 2) + '\n')
  const catalog = await readFile(options.catalogPath, 'utf8')
  const record = metadata.files.find(file => file.path === 'tools/manifest.json')
  if (record?.sha256 !== createHash('sha256').update(catalog).digest('hex')) throw new Error('The catalog differs from the metadata inventory')
  const envelope = await signToolChannel({ revision: options.revision, releaseVersion: options.releaseVersion, sourcePath: options.sourceOutput,
    metadataPath: options.metadataPath, catalogPath: options.catalogPath, privateKeyPath: options.privateKeyPath, publicKeysPath: options.publicKeysPath })
  await writeToolChannel(options.channelOutput, envelope, options.channelPaths)
  return { source, uploads }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2)
  const names = { '--metadata': 'metadataPath', '--archives': 'archives', '--catalog': 'catalogPath', '--version': 'releaseVersion',
    '--revision': 'revision', '--pieces': 'pieces', '--source-output': 'sourceOutput', '--output': 'channelOutput',
    '--previous-source': 'previousSource', '--key': 'privateKeyPath', '--public-keys': 'publicKeysPath' }
  if (args.length % 2) throw new Error(`Usage: build-tool-channel.mjs ${Object.keys(names).map(name => `${name} VALUE`).join(' ')}`)
  const options = {}
  for (let index = 0; index < args.length; index += 2) {
    const key = names[args[index]]
    if (key === undefined) throw new Error(`Unknown option: ${args[index]}`)
    options[key] = key === 'revision' ? Number(args[index + 1]) : args[index + 1]
  }
  const result = await buildToolChannel(options)
  console.log(JSON.stringify({ packId: result.source.packId, archives: result.source.archives.length, uploads: result.uploads }, null, 2))
}
