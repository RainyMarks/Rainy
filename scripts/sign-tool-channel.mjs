/** Sign a per-tool channel revision using the same builder identity as application resources. */
import { createHash, sign } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { readReleaseSigningKey, releasePublicKeys, releaseSigningKeyPath } from './release-signing-key.mjs'

const root = resolve(import.meta.dirname, '..')
/** Published channel fetched by RainyAgent 2.x (`TOOL_CHANNEL_URL` in src/main/native-tools-update.ts). */
export const TOOL_CHANNEL_PATH = resolve(root, 'toolpacks/native-tools-channel.v2.signed.json')
/** Byte-identical copy at the repository path that installed 1.x clients fetch. */
export const LEGACY_TOOL_CHANNEL_PATH = resolve(root, 'apps/rainy-desktop/toolpacks/native-tools-channel.v2.signed.json')

/**
 * Write a signed channel; writing the published channel also refreshes its 1.x copy.
 * @param {string} output Destination file.
 * @param {object} envelope Signed channel envelope.
 * @param {{published?: string, legacy?: string}} [paths] Published channel and its 1.x copy.
 * @returns {Promise<string[]>} Files written.
 */
export async function writeToolChannel(output, envelope, paths = {}) {
  const text = JSON.stringify(envelope) + '\n'
  await writeFile(output, text)
  if (resolve(output) !== resolve(paths.published ?? TOOL_CHANNEL_PATH)) return [output]
  const legacy = paths.legacy ?? LEGACY_TOOL_CHANNEL_PATH
  await mkdir(dirname(legacy), { recursive: true })
  await writeFile(legacy, text)
  return [output, legacy]
}

/**
 * Bind a catalog, installation inventory and downloadable archive selection to one publisher revision.
 * @param {{revision:number,releaseVersion:string,sourcePath:string,metadataPath:string,catalogPath:string,privateKeyPath?:string,publicKeysPath?:string}} options Prepared tool release inputs.
 * @returns {Promise<object>} Signed public envelope, with no private key bytes.
 */
export async function signToolChannel(options) {
  if (!Number.isSafeInteger(options.revision) || options.revision < 1 || !/^\d+\.\d+\.\d+$/u.test(options.releaseVersion)) throw new Error('Expected a positive channel revision and release version')
  const key = await readReleaseSigningKey(options.privateKeyPath ?? releaseSigningKeyPath(process.env.RAINY_RELEASE_SIGNING_KEY))
  const publicKeys = releasePublicKeys(key)
  const keyId = Object.keys(publicKeys.keys)[0]
  const trustedKeys = JSON.parse(await readFile(options.publicKeysPath ?? resolve(root, 'resources/native-tools-public-keys.json'), 'utf8'))
  if (trustedKeys.version !== 1 || trustedKeys.keys?.[keyId] !== publicKeys.keys[keyId]) throw new Error('The tool signing key does not match the public keys shipped to clients')
  const [source, metadata, catalog] = await Promise.all([
    readFile(options.sourcePath, 'utf8').then(JSON.parse), readFile(options.metadataPath, 'utf8').then(JSON.parse), readFile(options.catalogPath, 'utf8'),
  ])
  const record = metadata.files.find(file => file.path === 'tools/manifest.json')
  if (source.version !== 2 || metadata.version !== 2 || source.packId !== metadata.id || !record || record.bytes !== Buffer.byteLength(catalog)
    || record.sha256 !== createHash('sha256').update(catalog).digest('hex')) throw new Error('Tool-channel inputs do not match their frozen metadata')
  const payload = Buffer.from(JSON.stringify({ version: 2, revision: options.revision, releaseVersion: options.releaseVersion, keyId, source, metadata, catalog }))
  return { version: 2, payload: payload.toString('base64'), signature: sign(null, Buffer.concat([Buffer.from('RainyAgent/tool-channel/v2\0'), payload]), key).toString('base64') }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2)
  if (args.length % 2) throw new Error('Usage: sign-tool-channel.mjs --revision N --version VERSION --source FILE --metadata FILE --catalog FILE --output FILE [--key FILE]')
  const values = Object.fromEntries(Array.from({ length: args.length / 2 }, (_, index) => [args[index * 2], args[index * 2 + 1]]))
  for (const name of Object.keys(values)) if (!['--revision', '--version', '--source', '--metadata', '--catalog', '--output', '--key', '--public-keys'].includes(name)) throw new Error(`Unknown option: ${name}`)
  const envelope = await signToolChannel({ revision: Number(values['--revision']), releaseVersion: values['--version'], sourcePath: values['--source'],
    metadataPath: values['--metadata'], catalogPath: values['--catalog'], privateKeyPath: values['--key'], publicKeysPath: values['--public-keys'] })
  const written = await writeToolChannel(values['--output'], envelope)
  console.log(`Signed tool channel revision ${values['--revision']} for ${values['--version']}: ${written.join(', ')}`)
}
