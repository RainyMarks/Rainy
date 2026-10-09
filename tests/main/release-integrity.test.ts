/** Resource authenticity uses real build signatures and filesystem mutation evidence. */
import { createHash, generateKeyPairSync, sign } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { tmpdir } from 'node:os'
import { afterEach, expect, it } from 'vitest'
import {
  authenticateReleaseManifest, ensureReleaseIntegrity, ReleaseIntegrityError, RELEASE_SIGNATURE_DOMAIN, verifyReleaseResources,
} from '../../src/main/release-integrity.ts'
import { embeddedReleaseKeys, parseReleaseKeyring, releasePublicKeyId } from '../../src/main/release-trust.ts'

const directories: string[] = []
afterEach(async () => { await Promise.all(directories.splice(0).map(path => rm(path, { recursive: true, force: true, maxRetries: 5 }))) })

it.each([
  { name: 'host.js', contents: 'published host bytes' },
  { name: 'app-update.yml', contents: 'provider: github\nowner: RainyMarks\nrepo: RainyAgent\n' },
])('authenticates $name and refuses both changed bytes and forged inventory', async ({ name, contents }) => {
  const directory = await mkdtemp(join(tmpdir(), 'rainy-integrity-'))
  directories.push(directory)
  const { publicKey, privateKey } = generateKeyPairSync('ed25519')
  const pem = publicKey.export({ type: 'spki', format: 'pem' }).toString()
  const keyId = releasePublicKeyId(pem)
  const keys = parseReleaseKeyring({ version: 1, keys: { [keyId]: pem } })
  const bytes = Buffer.from(contents)
  await writeFile(join(directory, name), bytes)
  const input = { version: 1, product: 'RainyAgent', buildVersion: 'test', createdAt: new Date().toISOString(), keyId,
    files: [{ path: name, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') }] }
  const envelope = (value: object) => {
    const payload = Buffer.from(JSON.stringify(value))
    return { version: 1, payload: payload.toString('base64url'),
      signature: sign(null, Buffer.concat([Buffer.from(RELEASE_SIGNATURE_DOMAIN), payload]), privateKey).toString('base64url') }
  }
  const signed = envelope(input)
  const path = join(directory, 'release-manifest.signed.json')
  await writeFile(path, JSON.stringify(signed))
  expect((await verifyReleaseResources(directory, path, keys)).buildVersion).toBe('test')
  const forged = { ...signed, payload: Buffer.from(JSON.stringify({ ...input, buildVersion: 'forged' })).toString('base64url') }
  expect(() => authenticateReleaseManifest(forged, keys)).toThrow('签名')
  const changed = Buffer.from(bytes)
  changed.writeUInt8(changed.readUInt8(0) ^ 1, 0)
  await writeFile(join(directory, name), changed)
  await expect(verifyReleaseResources(directory, path, keys)).rejects.toThrow('资源')
  expect(JSON.parse(await readFile(path, 'utf8'))).toEqual(signed)
  expect(() => authenticateReleaseManifest(envelope({ ...input, files: [{ ...input.files[0], path: '../outside.js' }] }), keys)).toThrow()
})

async function signedRelease(files: Record<string, string>) {
  const directory = await mkdtemp(join(tmpdir(), 'rainy-integrity-'))
  directories.push(directory)
  const { publicKey, privateKey } = generateKeyPairSync('ed25519')
  const pem = publicKey.export({ type: 'spki', format: 'pem' }).toString()
  const keyId = releasePublicKeyId(pem)
  const keys = parseReleaseKeyring({ version: 1, keys: { [keyId]: pem } })
  const inventory = []
  for (const [name, contents] of Object.entries(files)) {
    await mkdir(dirname(join(directory, name)), { recursive: true })
    await writeFile(join(directory, name), contents)
    inventory.push({ path: name, bytes: Buffer.byteLength(contents), sha256: createHash('sha256').update(contents).digest('hex') })
  }
  const payload = Buffer.from(JSON.stringify({ version: 1, product: 'RainyAgent', buildVersion: 'test', createdAt: new Date().toISOString(),
    keyId, files: inventory }))
  const signedPath = join(directory, 'release-manifest.signed.json')
  await writeFile(signedPath, JSON.stringify({ version: 1, payload: payload.toString('base64url'),
    signature: sign(null, Buffer.concat([Buffer.from(RELEASE_SIGNATURE_DOMAIN), payload]), privateKey).toString('base64url') }))
  return { directory, keys, signedPath, stampPath: join(directory, 'user-data', 'release-verified.json') }
}

it('hashes an inventory once, then reuses its stamp until the inventory or root changes', async () => {
  const release = await signedRelease({ 'windows-host/app/dist/host.js': 'host', 'windows-host/node/node.exe': 'node' })
  let clock = 1000
  const options = { root: release.directory, signedPath: release.signedPath, keys: release.keys, stampPath: release.stampPath,
    recheckIntervalMs: 60_000, now: () => clock }
  const progress: number[] = []
  const first = await ensureReleaseIntegrity({ ...options, onProgress: (completed) => { progress.push(completed) } })
  expect(first.verified).toBe('full')
  expect(progress).toEqual([1, 2])
  expect(JSON.parse(await readFile(release.stampPath, 'utf8'))).toMatchObject({ version: 2, checkedAt: 1000 })
  const second = await ensureReleaseIntegrity(options)
  expect(second.verified).toBe('stamp')
  expect(await second.recheck()).toBe('skipped')
  clock += 60_000
  expect(await second.recheck()).toBe('unchanged')
  expect(JSON.parse(await readFile(release.stampPath, 'utf8'))).toMatchObject({ checkedAt: 61_000 })
  // A stamp written by an earlier release format is not trusted.
  await writeFile(release.stampPath, JSON.stringify({ version: 1, manifestSha256: 'a'.repeat(64), root: release.directory }))
  expect((await ensureReleaseIntegrity(options)).verified).toBe('full')
})

it('detects changed resources in the background check and verifies fully on the next launch', async () => {
  const release = await signedRelease({ 'host.js': 'published host bytes' })
  let clock = 0
  const options = { root: release.directory, signedPath: release.signedPath, keys: release.keys, stampPath: release.stampPath,
    recheckIntervalMs: 10, now: () => clock }
  await ensureReleaseIntegrity(options)
  const launched = await ensureReleaseIntegrity(options)
  await writeFile(join(release.directory, 'host.js'), 'tampered host bytes!')
  clock = 100
  await expect(launched.recheck()).rejects.toBeInstanceOf(ReleaseIntegrityError)
  await expect(readFile(release.stampPath, 'utf8')).rejects.toMatchObject({ code: 'ENOENT' })
  await expect(ensureReleaseIntegrity(options)).rejects.toThrow('发行资源')
  await rm(join(release.directory, 'host.js'))
  await expect(ensureReleaseIntegrity(options)).rejects.toThrow('发行资源缺失：host.js')
})

it('rejects empty, private, non-Ed25519, and mismatched public trust data', () => {
  expect(() => parseReleaseKeyring({ version: 1, keys: {} })).toThrow('缺少')
  expect(() => parseReleaseKeyring({ version: 1, keys: { ['a'.repeat(32)]: '-----BEGIN PRIVATE KEY-----\nfixture' } })).toThrow('只能包含发行公钥')
  const { publicKey } = generateKeyPairSync('ed25519')
  const pem = publicKey.export({ type: 'spki', format: 'pem' }).toString()
  expect(() => parseReleaseKeyring({ version: 1, keys: { ['a'.repeat(32)]: pem } })).toThrow('标识')
  const rsa = generateKeyPairSync('rsa', { modulusLength: 2048 }).publicKey.export({ type: 'spki', format: 'pem' }).toString()
  expect(() => releasePublicKeyId(rsa)).toThrow('Ed25519')
  expect(embeddedReleaseKeys()).toBeUndefined()
})
