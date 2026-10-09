/** Sign final packaged resources with the builder's independent release key. */
import { createHash, sign } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { lstat, readFile, readdir, writeFile } from 'node:fs/promises'
import { dirname, isAbsolute, join, relative, resolve } from 'node:path'
import { readReleaseSigningKey, releasePublicKeys, releaseSigningKeyPath } from './release-signing-key.mjs'

const app = resolve(import.meta.dirname, '..')
const signatureDomain = 'RainyAgent/release-manifest/v1\0'

/**
 * Match a builder's private key to the public key staged before compiling the carrier.
 * @param {string} path Existing private key file; never copied into resources.
 * @param {unknown} publicKeys Untrusted staged public-key JSON.
 * @returns {Promise<{key: import('node:crypto').KeyObject, keyId: string}>} Matching signing identity.
 */
export async function resolveReleaseSigner(path, publicKeys) {
  const key = await readReleaseSigningKey(path)
  const expected = releasePublicKeys(key)
  if (publicKeys === null || typeof publicKeys !== 'object' || publicKeys.version !== 1
    || publicKeys.keys === null || typeof publicKeys.keys !== 'object' || Array.isArray(publicKeys.keys)
    || JSON.stringify(Object.entries(publicKeys.keys)) !== JSON.stringify(Object.entries(expected.keys))) {
    throw new Error('The release signing key does not match the staged public keys')
  }
  return { key, keyId: Object.keys(expected.keys)[0] }
}

/**
 * Inventory final Electron resources and any generated updater configuration, then sign them.
 * @param {string} resourceRoot Final application resource directory.
 * @param {{privateKeyPath?: string}} [options] Explicit builder key, otherwise RAINY_RELEASE_SIGNING_KEY or the local build key.
 * @returns {Promise<{files: number, path: string}>} Signed inventory location and file count.
 */
export async function signReleaseResources(resourceRoot, options = {}) {
  const root = resolve(resourceRoot)
  const keyPath = options.privateKeyPath ?? releaseSigningKeyPath(process.env.RAINY_RELEASE_SIGNING_KEY)
  const relativeKey = relative(root, resolve(keyPath))
  if (relativeKey === '' || (!isAbsolute(relativeKey) && relativeKey !== '..'
    && !relativeKey.startsWith('..\\') && !relativeKey.startsWith('../'))) {
    throw new Error('The private release key must be outside packaged resources')
  }
  const keys = JSON.parse(await readFile(join(root, 'release-public-keys.json'), 'utf8'))
  const signer = await resolveReleaseSigner(keyPath, keys)
  const privateKeyDigest = createHash('sha256').update(await readFile(keyPath)).digest('hex')
  const packageInfo = JSON.parse(await readFile(join(app, 'package.json'), 'utf8'))
  const files = []
  async function inventory(path) {
    const info = await lstat(path)
    if (info.isSymbolicLink()) throw new Error(`Release resource cannot be a symbolic link: ${path}`)
    if (info.isDirectory()) {
      for (const name of (await readdir(path)).sort()) await inventory(join(path, name))
      return
    }
    if (!info.isFile()) throw new Error(`Unexpected release resource: ${path}`)
    const hash = createHash('sha256')
    for await (const chunk of createReadStream(path)) hash.update(chunk)
    const sha256 = hash.digest('hex')
    if (sha256 === privateKeyDigest) throw new Error('A private release key was copied into packaged resources')
    files.push({ path: relative(root, path).replaceAll('\\', '/'), sha256, bytes: info.size })
  }
  for (const name of ['windows-host', 'release-public-keys.json', 'environment-component-catalog.json', 'install-environment-component.py',
    'linux-runtime.json', 'install-runtime.py', 'optional-modules.json', 'native-tools-channel.signed.json', 'native-tools-public-keys.json', 'icon.ico']) {
    await inventory(join(root, name))
  }
  const updaterPath = join(root, 'app-update.yml')
  let updater
  try { updater = await lstat(updaterPath) }
  catch (error) { if (error.code !== 'ENOENT') throw error }
  if (updater) {
    if (!updater.isFile()) throw new Error('The generated app-update.yml must be a regular file')
    await inventory(updaterPath)
  }
  const windowsInventory = JSON.parse(await readFile(join(root, 'windows-host/runtime.json'), 'utf8'))
  const expectedWindows = new Map(windowsInventory.files.map(file => [file.path, file]))
  const copiedWindows = files.filter(file => file.path.startsWith('windows-host/') && file.path !== 'windows-host/runtime.json')
  if (copiedWindows.length !== expectedWindows.size) throw new Error('The packaged Windows Host file count differs from its verified inventory')
  for (const file of copiedWindows) {
    const expected = expectedWindows.get(file.path.slice('windows-host/'.length))
    if (!expected || expected.bytes !== file.bytes || expected.sha256 !== file.sha256) {
      throw new Error(`Packaged Windows Host file differs from staging: ${file.path}`)
    }
  }
  const bytes = Buffer.from(JSON.stringify({ version: 1, product: 'RainyAgent', buildVersion: packageInfo.version,
    createdAt: new Date().toISOString(), keyId: signer.keyId, files }))
  const signed = { version: 1, payload: bytes.toString('base64url'),
    signature: sign(null, Buffer.concat([Buffer.from(signatureDomain), bytes]), signer.key).toString('base64url') }
  const target = join(root, 'release-manifest.signed.json')
  await writeFile(target, JSON.stringify(signed) + '\n')
  const evidence = { version: packageInfo.version, files: files.length, signedManifest: target,
    manifestSha256: createHash('sha256').update(await readFile(target)).digest('hex'), authenticode: process.env.CSC_LINK ? 'configured' : 'not-configured' }
  await writeFile(join(dirname(root), 'release-integrity.json'), JSON.stringify(evidence, null, 2) + '\n')
  return { files: files.length, path: target }
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  if (!process.argv[2]) throw new Error('Pass the final packaged resource directory')
  console.log(JSON.stringify(await signReleaseResources(process.argv[2])))
}
