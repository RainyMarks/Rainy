/** A publisher key mismatch must stop before a tool channel is written. */
import assert from 'node:assert/strict'
import { createHash, generateKeyPairSync, verify } from 'node:crypto'
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { test } from 'node:test'
import { releasePublicKeys } from '../scripts/release-signing-key.mjs'
import { signToolChannel } from '../scripts/sign-tool-channel.mjs'

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
