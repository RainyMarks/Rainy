/** Tool-channel signing refuses mismatched inputs and keys; publication keeps the 1.x copy identical. */
import assert from 'node:assert/strict'
import { createHash, generateKeyPairSync, verify } from 'node:crypto'
import { access, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { test } from 'node:test'
import { releasePublicKeys } from '../../scripts/release-signing-key.mjs'
import { LEGACY_TOOL_CHANNEL_PATH, TOOL_CHANNEL_PATH, signToolChannel, writeToolChannel } from '../../scripts/sign-tool-channel.mjs'

test('tool publisher uses the pinned public identity and a separate signature domain', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rainy-tool-channel-'))
  try {
    const keys = generateKeyPairSync('ed25519')
    const other = generateKeyPairSync('ed25519')
    const privateKeyPath = join(root, 'publisher.pem')
    const publicKeysPath = join(root, 'public.json')
    const catalogPath = join(root, 'catalog.json')
    const metadataPath = join(root, 'metadata.json')
    const sourcePath = join(root, 'source.json')
    const catalog = '{"version":1,"tools":[]}\n'
    const sha256 = createHash('sha256').update(catalog).digest('hex')
    await Promise.all([
      writeFile(privateKeyPath, keys.privateKey.export({ format: 'pem', type: 'pkcs8' }), { mode: 0o600 }),
      writeFile(publicKeysPath, JSON.stringify(releasePublicKeys(keys.privateKey))),
      writeFile(catalogPath, catalog),
      writeFile(metadataPath, JSON.stringify({ version: 2, id: 'a'.repeat(64), files: [{ path: 'tools/manifest.json', bytes: Buffer.byteLength(catalog), sha256 }] })),
      writeFile(sourcePath, JSON.stringify({ version: 2, packId: 'a'.repeat(64) })),
    ])
    const options = { revision: 1, releaseVersion: '1.0.0', privateKeyPath, publicKeysPath, catalogPath, metadataPath, sourcePath }
    const envelope = await signToolChannel(options)
    const payload = Buffer.from(envelope.payload, 'base64')
    const publicPem = Object.values(releasePublicKeys(keys.privateKey).keys)[0]
    assert.equal(verify(null, Buffer.concat([Buffer.from('RainyAgent/tool-channel/v2\0'), payload]), publicPem,
      Buffer.from(envelope.signature, 'base64')), true)
    await writeFile(sourcePath, JSON.stringify({ version: 1, packId: 'a'.repeat(64) }))
    await assert.rejects(signToolChannel(options), /do not match their frozen metadata/)
    await writeFile(sourcePath, JSON.stringify({ version: 2, packId: 'a'.repeat(64) }))
    await writeFile(publicKeysPath, JSON.stringify(releasePublicKeys(other.privateKey)))
    await assert.rejects(signToolChannel(options), /does not match the public keys/)
    assert.equal((await readFile(catalogPath, 'utf8')), catalog)
  } finally {
    if (relative(tmpdir(), root).startsWith('..')) throw new Error('Test cleanup escaped temporary storage')
    await rm(root, { recursive: true, force: true })
  }
})

test('writing the published channel also writes the copy that 1.x clients fetch', async () => {
  const root = await mkdtemp(join(tmpdir(), 'rainy-tool-channel-'))
  try {
    const paths = { published: join(root, 'toolpacks/channel.json'), legacy: join(root, 'apps/rainy-desktop/toolpacks/channel.json') }
    const envelope = { version: 2, payload: 'cGF5bG9hZA==', signature: 'c2lnbmF0dXJl' }
    const scratch = join(root, 'scratch.json')
    assert.deepEqual(await writeToolChannel(scratch, envelope, paths), [scratch])
    await assert.rejects(access(paths.legacy))
    await mkdir(join(root, 'toolpacks'))
    assert.deepEqual(await writeToolChannel(paths.published, envelope, paths), [paths.published, paths.legacy])
    assert.equal(await readFile(paths.legacy, 'utf8'), JSON.stringify(envelope) + '\n')
    assert.equal(await readFile(paths.legacy, 'utf8'), await readFile(paths.published, 'utf8'))
  } finally {
    if (relative(tmpdir(), root).startsWith('..')) throw new Error('Test cleanup escaped temporary storage')
    await rm(root, { recursive: true, force: true })
  }
})

test('the repository publishes the same channel bytes at the 2.x and 1.x locations', async () => {
  assert.deepEqual(await readFile(LEGACY_TOOL_CHANNEL_PATH), await readFile(TOOL_CHANNEL_PATH))
})
