/** Release-resource checks reject missing and altered offline development media. */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, relative, isAbsolute } from 'node:path'
import { verifyIdeResources } from '../scripts/verify-ide-resources.mjs'

async function fixture(t) {
  const app = await mkdtemp(join(tmpdir(), 'rainy-ide-resources-'))
  t.after(async () => {
    const child = relative(tmpdir(), app)
    assert(child && !child.startsWith('..') && !isAbsolute(child))
    await rm(app, { recursive: true })
  })
  for (const dir of ['toolpacks', 'scripts', 'resources/ide/system-packages']) await mkdir(join(app, dir), { recursive: true })
  const hash = value => createHash('sha256').update(value).digest('hex')
  const licenses = [{ id: 'fixture-license', version: '1', url: 'https://example.invalid/LICENSE', path: 'LICENSE.txt', bytes: 7, sha256: hash('license') }]
  const source = JSON.stringify({ assets: [], licenses, ubuntuBaseline: '26.04', systemPackages: ['compiler'] })
  await writeFile(join(app, 'toolpacks/ide-tools.sources.json'), source)
  for (const name of ['prepare-ide-tools.py', 'install-ide-system-packages.py']) await writeFile(join(app, 'scripts', name), 'fixture')
  await writeFile(join(app, 'resources/ide/helper'), 'helper')
  await writeFile(join(app, 'resources/ide/LICENSE.txt'), 'license')
  await writeFile(join(app, 'resources/ide/manifest.json'), JSON.stringify({ version: 1, assets: [], licenses, sourceSha256: hash(source),
    preparerSha256: hash('fixture'), installerSourceSha256: hash('fixture'), required: ['helper', 'LICENSE.txt'],
    files: [{ path: 'helper', bytes: 6, sha256: hash('helper') }, { path: 'LICENSE.txt', bytes: 7, sha256: hash('license') }] }))
  const system = join(app, 'resources/ide/system-packages')
  await writeFile(join(system, 'compiler.deb'), 'deb')
  await writeFile(join(system, 'manifest.json'), JSON.stringify({ version: 1, os: 'ubuntu', osVersion: '26.04', architecture: 'amd64',
    requestedPackages: ['compiler'], packages: [{ file: 'compiler.deb', bytes: 3, sha256: hash('deb') }] }))
  return app
}

test('complete matching helper and system inventories permit packaging', async t => {
  assert.deepEqual(await verifyIdeResources(await fixture(t)), { helpers: 2, systemPackages: 1, bytes: 16 })
})
test('a changed system package stops packaging even when its length is unchanged', async t => {
  const app = await fixture(t)
  await writeFile(join(app, 'resources/ide/system-packages/compiler.deb'), 'bad')
  await assert.rejects(verifyIdeResources(app), /compiler\.deb/)
})
test('a missing helper or stale preparation source stops packaging', async t => {
  const app = await fixture(t)
  await writeFile(join(app, 'scripts/prepare-ide-tools.py'), 'changed')
  await assert.rejects(verifyIdeResources(app))
  await writeFile(join(app, 'scripts/prepare-ide-tools.py'), 'fixture')
  await rm(join(app, 'resources/ide/helper'))
  await assert.rejects(verifyIdeResources(app), /ENOENT/)
})

test('an omitted pinned license or a substituted notice stops packaging', async t => {
  const app = await fixture(t)
  const path = join(app, 'resources/ide/manifest.json')
  const manifest = JSON.parse(await readFile(path, 'utf8'))
  await writeFile(path, JSON.stringify({ ...manifest, files: manifest.files.filter(row => row.path !== 'LICENSE.txt') }))
  await assert.rejects(verifyIdeResources(app), /license|LICENSE/)
  const substituted = 'changed'
  await writeFile(join(app, 'resources/ide/LICENSE.txt'), substituted)
  await writeFile(path, JSON.stringify({ ...manifest, files: manifest.files.map(row => row.path === 'LICENSE.txt'
    ? { ...row, sha256: createHash('sha256').update(substituted).digest('hex') } : row) }))
  await assert.rejects(verifyIdeResources(app), /LICENSE/)
})
