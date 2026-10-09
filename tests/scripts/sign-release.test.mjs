/** Release signing uses a builder-owned key and validates the copied Host inventory. */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash, generateKeyPairSync, verify } from 'node:crypto'
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, relative } from 'node:path'
import { prepareReleaseKey, readReleaseSigningKey, releaseSigningKeyPath } from '../../scripts/release-signing-key.mjs'
import { resolveReleaseSigner, signReleaseResources } from '../../scripts/sign-release.mjs'

async function fixture(t) {
  const directory = await mkdtemp(join(tmpdir(), 'rainy-release-signing-'))
  t.after(async () => {
    const inside = relative(tmpdir(), directory)
    assert(inside && inside !== '..' && !inside.startsWith('..\\') && !inside.startsWith('../') && !isAbsolute(inside))
    await rm(directory, { recursive: true, force: true, maxRetries: 5 })
  })
  const root = join(directory, 'resources')
  const privateKeyPath = join(directory, 'builder', 'key.pem')
  const publicKeyPath = join(root, 'release-public-keys.json')
  const keys = await prepareReleaseKey({ privateKeyPath, publicKeyPath, create: true })
  return { directory, root, privateKeyPath, publicKeyPath, keys }
}

async function resources(f) {
  const bytes = Buffer.from('host fixture')
  await mkdir(join(f.root, 'windows-host'), { recursive: true })
  await writeFile(join(f.root, 'windows-host/host.js'), bytes)
  await writeFile(join(f.root, 'windows-host/runtime.json'), JSON.stringify({ files: [
    { path: 'host.js', bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') },
  ] }))
  for (const name of ['environment-component-catalog.json', 'install-environment-component.py', 'linux-runtime.json', 'install-runtime.py',
    'optional-modules.json', 'native-tools-channel.signed.json', 'native-tools-public-keys.json', 'icon.ico']) await writeFile(join(f.root, name), name)
}

test('local builds create a reusable key and distribute only its public half', async t => {
  const f = await fixture(t)
  const original = await readFile(f.privateKeyPath)
  assert.deepEqual(await prepareReleaseKey({ ...f, create: true }), f.keys)
  assert.deepEqual(await readFile(f.privateKeyPath), original)
  assert(!(await readFile(f.publicKeyPath, 'utf8')).includes('PRIVATE KEY'))
  assert.equal((await resolveReleaseSigner(f.privateKeyPath, f.keys)).keyId, Object.keys(f.keys.keys)[0])
  assert.throws(() => releaseSigningKeyPath(''), /must name/)
  await assert.rejects(prepareReleaseKey({ privateKeyPath: f.privateKeyPath, publicKeyPath: f.privateKeyPath, create: true }), /separate files/)
})

test('an explicit missing or invalid key is rejected without replacement', async t => {
  const f = await fixture(t)
  const missing = join(f.directory, 'missing.pem')
  await assert.rejects(prepareReleaseKey({ privateKeyPath: missing, publicKeyPath: f.publicKeyPath, create: false }), { code: 'ENOENT' })
  assert(!(await readdir(f.directory)).includes('missing.pem'))
  await assert.rejects(readReleaseSigningKey(f.directory), /regular, non-link file/)
  const invalid = join(f.directory, 'rsa.pem')
  const { privateKey } = generateKeyPairSync('rsa', { modulusLength: 2048 })
  await writeFile(invalid, privateKey.export({ format: 'pem', type: 'pkcs8' }))
  await assert.rejects(readReleaseSigningKey(invalid), /Ed25519/)
})

test('packaged resources receive a verifiable signature without a license manager', async t => {
  const f = await fixture(t)
  await resources(f)
  const result = await signReleaseResources(f.root, { privateKeyPath: f.privateKeyPath })
  const envelope = JSON.parse(await readFile(result.path, 'utf8'))
  const bytes = Buffer.from(envelope.payload, 'base64url')
  assert(verify(null, Buffer.concat([Buffer.from('RainyAgent/release-manifest/v1\0'), bytes]), Object.values(f.keys.keys)[0],
    Buffer.from(envelope.signature, 'base64url')))
  const manifest = JSON.parse(bytes.toString())
  assert.equal(manifest.files.length, 11)
  assert.equal(result.files, 11)
  assert(manifest.files.every(row => !/private|issuer|license-verifier/u.test(row.path)))
})

test('generated updater configuration is included in the signed resource inventory', async t => {
  const f = await fixture(t)
  await resources(f)
  const updater = Buffer.from('provider: github\nowner: RainyMarks\nrepo: RainyAgent\n')
  await writeFile(join(f.root, 'app-update.yml'), updater)
  const result = await signReleaseResources(f.root, { privateKeyPath: f.privateKeyPath })
  const envelope = JSON.parse(await readFile(result.path, 'utf8'))
  const payload = Buffer.from(envelope.payload, 'base64url')
  assert(verify(null, Buffer.concat([Buffer.from('RainyAgent/release-manifest/v1\0'), payload]), Object.values(f.keys.keys)[0],
    Buffer.from(envelope.signature, 'base64url')))
  const manifest = JSON.parse(payload)
  assert.equal(result.files, 12)
  assert.deepEqual(manifest.files.find(file => file.path === 'app-update.yml'), {
    path: 'app-update.yml', bytes: updater.length, sha256: createHash('sha256').update(updater).digest('hex'),
  })
})

test('an updater configuration directory prevents resource signing', async t => {
  const f = await fixture(t)
  await resources(f)
  await mkdir(join(f.root, 'app-update.yml'))
  await assert.rejects(signReleaseResources(f.root, { privateKeyPath: f.privateKeyPath }), /app-update.yml must be a regular file/)
  assert(!(await readdir(f.root)).includes('release-manifest.signed.json'))
})

test('components that are downloaded on demand are not part of the signed inventory', async t => {
  const f = await fixture(t)
  await resources(f)
  for (const path of ['strata-runtime/engine/strata.exe', 'php/php.exe']) {
    await mkdir(join(f.root, path, '..'), { recursive: true })
    await writeFile(join(f.root, path), 'left by an earlier installer')
  }
  const result = await signReleaseResources(f.root, { privateKeyPath: f.privateKeyPath })
  const envelope = JSON.parse(await readFile(result.path, 'utf8'))
  const manifest = JSON.parse(Buffer.from(envelope.payload, 'base64url'))
  assert.deepEqual(manifest.files.filter(file => /^(strata-runtime|php)\//u.test(file.path)), [])
  assert.deepEqual(manifest.files.filter(file => file.path.endsWith('.json') && !file.path.includes('/')).map(file => file.path).sort(), [
    'environment-component-catalog.json', 'linux-runtime.json', 'native-tools-channel.signed.json', 'native-tools-public-keys.json',
    'optional-modules.json', 'release-public-keys.json'])
})

test('a mismatched key or a key within resources cannot sign a package', async t => {
  const f = await fixture(t)
  const other = await fixture(t)
  await assert.rejects(signReleaseResources(f.root, { privateKeyPath: other.privateKeyPath }), /does not match/)
  await assert.rejects(signReleaseResources(f.root, { privateKeyPath: join(f.root, 'private.pem') }), /outside packaged resources/)
  assert.deepEqual(await readdir(f.root), ['release-public-keys.json'])
})

test('a changed or missing Host resource prevents signing', async t => {
  const f = await fixture(t)
  await resources(f)
  await writeFile(join(f.root, 'windows-host/host.js'), 'changed host')
  await assert.rejects(signReleaseResources(f.root, { privateKeyPath: f.privateKeyPath }), /differs from staging/)
  await rm(join(f.root, 'windows-host/host.js'))
  await assert.rejects(signReleaseResources(f.root, { privateKeyPath: f.privateKeyPath }), /file count differs/)
  assert(!(await readdir(f.root)).includes('release-manifest.signed.json'))
})

test('a private key copied under a public resource name prevents signing', async t => {
  const f = await fixture(t)
  await resources(f)
  await writeFile(join(f.root, 'icon.ico'), await readFile(f.privateKeyPath))
  await assert.rejects(signReleaseResources(f.root, { privateKeyPath: f.privateKeyPath }), /private release key was copied/)
  assert(!(await readdir(f.root)).includes('release-manifest.signed.json'))
})
