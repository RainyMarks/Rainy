/** Build-owned Ed25519 keys; only the public keyring enters application resources. */
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync } from 'node:crypto'
import { lstat, mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'

const app = resolve(import.meta.dirname, '..')

/**
 * Resolve a release key file without falling back from an invalid explicit value.
 * @param {string | undefined} configured Existing PEM file, or undefined for the ignored build directory.
 * @returns {string} Absolute private-key path.
 */
export function releaseSigningKeyPath(configured) {
  if (configured !== undefined && configured.trim() === '') throw new Error('RAINY_RELEASE_SIGNING_KEY must name a private PEM file')
  return configured === undefined ? resolve(app, 'build/release-signing-key.pem') : resolve(configured)
}

/**
 * Read an existing Ed25519 PEM file without creating or replacing it.
 * @param {string} path Private-key file owned by the builder.
 * @returns {Promise<import('node:crypto').KeyObject>} Parsed signing key.
 */
export async function readReleaseSigningKey(path) {
  const info = await lstat(path)
  if (!info.isFile() || info.isSymbolicLink()) throw new Error('The release signing key must be a regular, non-link file')
  const key = createPrivateKey(await readFile(path))
  if (key.asymmetricKeyType !== 'ed25519') throw new Error('The release signing key must use Ed25519')
  return key
}

/**
 * Derive the distributed public keyring from a signing key.
 * @param {import('node:crypto').KeyObject} privateKey Builder-owned Ed25519 key.
 * @returns {{version: 1, keys: Record<string, string>}} Public-only trust data.
 */
export function releasePublicKeys(privateKey) {
  const publicKey = createPublicKey(privateKey)
  const id = createHash('sha256').update(publicKey.export({ format: 'der', type: 'spki' })).digest('hex').slice(0, 32)
  return { version: 1, keys: { [id]: publicKey.export({ format: 'pem', type: 'spki' }).toString() } }
}

/**
 * Stage public trust data; create a local build key only when explicitly allowed.
 * @param {{privateKeyPath: string, publicKeyPath: string, create: boolean}} options Separate private and public destinations.
 * @returns {Promise<{version: 1, keys: Record<string, string>}>} Public-only trust data.
 */
export async function prepareReleaseKey(options) {
  if (resolve(options.privateKeyPath) === resolve(options.publicKeyPath)) throw new Error('Private and public release keys need separate files')
  if (options.create) {
    await mkdir(dirname(options.privateKeyPath), { recursive: true })
    const { privateKey } = generateKeyPairSync('ed25519')
    try {
      await writeFile(options.privateKeyPath, privateKey.export({ format: 'pem', type: 'pkcs8' }), { flag: 'wx', mode: 0o600 })
    } catch (error) {
      if (!(error instanceof Error) || !('code' in error) || error.code !== 'EEXIST') throw error
    }
  }
  const keys = releasePublicKeys(await readReleaseSigningKey(options.privateKeyPath))
  await mkdir(dirname(options.publicKeyPath), { recursive: true })
  await writeFile(options.publicKeyPath, JSON.stringify(keys, null, 2) + '\n')
  return keys
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  const configured = process.env.RAINY_RELEASE_SIGNING_KEY
  const keys = await prepareReleaseKey({ privateKeyPath: releaseSigningKeyPath(configured),
    publicKeyPath: resolve(app, 'runtime/release-public-keys.json'), create: configured === undefined })
  console.log(`Prepared public release key ${Object.keys(keys.keys)[0]}`)
}
