/** Sign final packaged resources with the builder's independent release key. */
import { createHash, sign } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { lstat, readFile, readdir, writeFile } from 'node:fs/promises'
import { isAbsolute, join, relative, resolve } from 'node:path'
import { readReleaseSigningKey, releasePublicKeys, releaseSigningKeyPath } from './release-signing-key.mjs'

const app = resolve(import.meta.dirname, '..')
const signatureDomain = 'RainyAgent/release-manifest/v1\0'
/** The signed resources besides `windows-host`, in the order 3.x lists them. */
export const SIGNED_RESOURCES = ['release-public-keys.json', 'environment-component-catalog.json', 'install-environment-component.py',
  'linux-runtime.json', 'linux-app.tar.gz', 'install-runtime.py', 'optional-modules.json', 'icon.ico']

/**
 * Whether `path` lies outside `root` (not equal to it and not below it).
 * @param {string} root Folder.
 * @param {string} path Absolute or relative path.
 * @returns {boolean} True when the path is elsewhere.
 */
function outside(root, path) {
  const child = relative(resolve(root), resolve(path))
  return child !== '' && (isAbsolute(child) || child === '..' || child.startsWith('..\\') || child.startsWith('../'))
}

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
 * Inventory the final resources of a Rainy 4 package and sign them. 4.0 has no Electron updater, so a 3.x
 * `app-update.yml` refuses signing, and no evidence file is written into the package (3.x's release-integrity.json
 * carried the builder's paths).
 * @param {string} resourceRoot Final application resource directory (`<package>\resources`).
 * @param {{privateKeyPath?: string, version?: string, packageRoot?: string}} [options] Explicit builder key (otherwise
 * RAINY_RELEASE_SIGNING_KEY or the local build key), the version the list records (otherwise package.json's) and the
 * folder holding the whole package, which the key must lie outside as well.
 * @returns {Promise<{files: number, path: string, version: string, manifestSha256: string}>} Signed inventory and evidence.
 */
export async function signReleaseResources(resourceRoot, options = {}) {
  const root = resolve(resourceRoot)
  const keyPath = options.privateKeyPath ?? releaseSigningKeyPath(process.env.RAINY_RELEASE_SIGNING_KEY)
  if (!outside(root, keyPath)) throw new Error('The private release key must be outside packaged resources')
  if (options.packageRoot !== undefined && !outside(options.packageRoot, keyPath)) throw new Error('The private release key must be outside the package')
  if ((await lstat(root)).isSymbolicLink()) throw new Error(`Release resources cannot be a symbolic link: ${root}`)
  const keys = JSON.parse(await readFile(join(root, 'release-public-keys.json'), 'utf8'))
  const signer = await resolveReleaseSigner(keyPath, keys)
  const privateKeyDigest = createHash('sha256').update(await readFile(keyPath)).digest('hex')
  const version = options.version ?? JSON.parse(await readFile(join(app, 'package.json'), 'utf8')).version
  try {
    await lstat(join(root, 'app-update.yml'))
    throw new Error('app-update.yml belongs to the 3.x Electron updater and cannot be part of a Rainy 4 package')
  } catch (error) {
    if (error.code !== 'ENOENT') throw error
  }
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
  for (const name of ['windows-host', ...SIGNED_RESOURCES]) await inventory(join(root, name))
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
  const bytes = Buffer.from(JSON.stringify({ version: 1, product: 'RainyAgent', buildVersion: version,
    createdAt: new Date().toISOString(), keyId: signer.keyId, files }))
  const signed = { version: 1, payload: bytes.toString('base64url'),
    signature: sign(null, Buffer.concat([Buffer.from(signatureDomain), bytes]), signer.key).toString('base64url') }
  const target = join(root, 'release-manifest.signed.json')
  await writeFile(target, JSON.stringify(signed) + '\n')
  return { files: files.length, path: target, version, manifestSha256: createHash('sha256').update(await readFile(target)).digest('hex') }
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  if (!process.argv[2]) throw new Error('Pass the final packaged resource directory')
  console.log(JSON.stringify(await signReleaseResources(process.argv[2])))
}
