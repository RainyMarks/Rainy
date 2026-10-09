/** Release transport preserves file bytes and rejects ambiguous or changed inputs. */
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, relative, resolve } from 'node:path'
import * as tar from 'tar'
import { extendReleaseAssets, packReleaseAssets, validateReleaseManifest } from '../scripts/release-assets.mjs'
import { bootstrapReleaseInputs, extractBuildInput, restoreReleaseInputs, writeReleaseComponentCatalog } from '../scripts/bootstrap-release-inputs.mjs'

const run = promisify(execFile)
const baseUrl = 'https://github.com/RainyMarks/RainyAgent/releases/download/v1.0.0/'
const reassembler = resolve(import.meta.dirname, '../scripts/reassemble-release.ps1')

async function fixture(t) {
  const root = await mkdtemp(join(tmpdir(), 'rainy-release-assets-'))
  t.after(async () => {
    const child = relative(tmpdir(), root)
    assert(child && !child.startsWith('..') && !isAbsolute(child))
    await rm(root, { recursive: true, force: true, maxRetries: 5 })
  })
  const source = join(root, 'source')
  await mkdir(source)
  async function put(path, value) { await mkdir(join(source, path, '..'), { recursive: true }); await writeFile(join(source, path), value) }
  await put('RainyAgent-1.0.0-windows-x64-setup.exe', 'installer')
  await put('environment-components/data.tar.gz', Buffer.from([0, 255, 1, 2, 3, 4, 5, 6]))
  await put('说明.txt', '测试\n')
  const files = [
    { path: 'RainyAgent-1.0.0-windows-x64-setup.exe', category: 'installer' },
    { path: 'environment-components/data.tar.gz', category: 'offline' },
    { path: '说明.txt', category: 'offline' },
  ]
  const output = join(root, 'parts')
  const manifest = await packReleaseAssets({ sourceRoot: source, outputDirectory: output, files, releaseVersion: '1.0.0', baseUrl, partBytes: 3 })
  return { root, source, output, files, manifest, put }
}

test('pieces are deterministic and retain a raw installer and original file hashes', async t => {
  const f = await fixture(t)
  const other = join(f.root, 'other-parts')
  const second = await packReleaseAssets({ sourceRoot: f.source, outputDirectory: other, files: [...f.files].reverse(), releaseVersion: '1.0.0', baseUrl, partBytes: 3 })
  assert.deepEqual(second, f.manifest)
  for (const entry of f.manifest.files) {
    const actual = Buffer.concat(await Promise.all(entry.pieces.map(piece => readFile(join(f.output, piece.file)))))
    assert.deepEqual(actual, await readFile(join(f.source, entry.path)))
    if (entry.category === 'installer') assert.equal(entry.pieces[0].file, entry.path)
    else assert(entry.pieces.every(piece => piece.bytes <= 3))
  }
})

test('raw updater assets extend the frozen input manifest without consuming baseline pieces', async t => {
  const f = await fixture(t)
  const baseline = structuredClone(f.manifest)
  await f.put('latest.yml', 'version: 1.0.0\n')
  await f.put('RainyAgent-1.0.0-windows-x64-setup.exe.blockmap', 'blockmap')
  const piece = f.manifest.files.find(entry => entry.category === 'offline').pieces[0]
  await rm(join(f.output, piece.file))
  const manifest = await extendReleaseAssets({ manifest: baseline, sourceRoot: f.source, outputDirectory: f.output,
    files: ['latest.yml', 'RainyAgent-1.0.0-windows-x64-setup.exe.blockmap'].map(path => ({ path, category: 'installer' })) })
  for (const entry of baseline.files) assert.deepEqual(manifest.files.find(item => item.path === entry.path), entry)
  assert.equal(await readFile(join(f.output, 'latest.yml'), 'utf8'), 'version: 1.0.0\n')
  assert.equal(manifest.files.length, baseline.files.length + 2)
  assert.deepEqual(f.manifest, baseline)
  await assert.rejects(extendReleaseAssets({ manifest: baseline, sourceRoot: f.source, outputDirectory: f.output,
    files: [{ path: 'latest.yml', category: 'installer' }] }), /does not match/)
})

test('manifest rejects traversal, Windows aliases, case collisions, and overlapping paths', async t => {
  const f = await fixture(t)
  for (const name of ['../escape', 'C:/escape', 'folder\\escape', 'NUL.txt', 'file.', 'folder//file']) {
    const value = structuredClone(f.manifest)
    value.files[0].path = name
    assert.throws(() => validateReleaseManifest(value), /Unsafe release path/)
  }
  for (const path of ['说明.TXT', '说明.txt/child']) {
    const value = structuredClone(f.manifest)
    const other = structuredClone(value.files.at(-1))
    other.path = path
    value.files.push(other)
    assert.throws(() => validateReleaseManifest(value), /collision/)
  }
  const value = structuredClone(f.manifest)
  value.files[1].pieces[0].file = value.files[0].pieces[0].file
  assert.throws(() => validateReleaseManifest(value), /Invalid release piece/)
  for (const unpinned of ['http://example.test/release/', 'https://example.test/latest/download/', 'https://example.test/release/?token=secret']) {
    const changed = structuredClone(f.manifest)
    changed.files[1].baseUrl = unpinned
    assert.throws(() => validateReleaseManifest(changed), /pinned HTTPS directory/)
  }
})

test('local bootstrap restores only verified inputs and preserves different destination files', async t => {
  const f = await fixture(t)
  const destination = join(f.root, 'restored')
  await restoreReleaseInputs({ manifest: f.manifest, inputsDirectory: f.source, outputDirectory: destination })
  assert.deepEqual(await readFile(join(destination, 'environment-components/data.tar.gz')), await readFile(join(f.source, 'environment-components/data.tar.gz')))
  await assert.rejects(readFile(join(destination, f.files[0].path)), { code: 'ENOENT' })
  await writeFile(join(destination, '说明.txt'), 'existing user bytes')
  await assert.rejects(restoreReleaseInputs({ manifest: f.manifest, inputsDirectory: f.source, outputDirectory: destination }), /checksum/)
  assert.equal(await readFile(join(destination, '说明.txt'), 'utf8'), 'existing user bytes')
  await writeFile(join(f.source, 'environment-components/data.tar.gz'), 'corrupted')
  await assert.rejects(restoreReleaseInputs({ manifest: f.manifest, inputsDirectory: f.source, outputDirectory: join(f.root, 'bad-input') }), /checksum/)
})

test('download bootstrap verifies each piece and never publishes a corrupt reconstruction', async t => {
  const f = await fixture(t)
  const updatedUrl = 'https://github.com/RainyMarks/RainyAgent/releases/download/v1.0.2-resources/'
  f.manifest.files.find(entry => entry.path === '说明.txt').baseUrl = updatedUrl
  let changed = false
  const requested = []
  t.mock.method(globalThis, 'fetch', async url => {
    const name = decodeURIComponent(new URL(url).pathname.split('/').at(-1))
    const entry = f.manifest.files.find(item => item.pieces.some(piece => piece.file === name))
    assert.equal(new URL(url).href.startsWith(entry.baseUrl ?? baseUrl), true)
    requested.push(name)
    const bytes = await readFile(join(f.output, name))
    return new Response(changed ? Buffer.alloc(bytes.length, 42) : bytes)
  })
  t.after(() => t.mock.restoreAll())
  const output = join(f.root, 'downloaded')
  await restoreReleaseInputs({ manifest: f.manifest, outputDirectory: output })
  for (const entry of f.manifest.files.filter(item => item.category !== 'installer')) assert.deepEqual(await readFile(join(output, entry.path)), await readFile(join(f.source, entry.path)))
  assert(requested.length > 1)
  changed = true
  const badOutput = join(f.root, 'bad-download')
  await assert.rejects(restoreReleaseInputs({ manifest: f.manifest, outputDirectory: badOutput }), /piece checksum/)
  await assert.rejects(readFile(join(badOutput, 'environment-components/data.tar.gz')), { code: 'ENOENT' })
})

test('PowerShell reassembles binary and Unicode files and rejects a modified or missing piece', { skip: process.platform !== 'win32' ? 'Windows PowerShell is the release entry point' : false }, async t => {
  const f = await fixture(t)
  const invoke = output => run('powershell.exe', ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', reassembler,
    '-Manifest', join(f.output, 'release-assets.json'), '-PartsDirectory', f.output, '-OutputDirectory', output], { windowsHide: true })
  const restored = join(f.root, 'ps-restored')
  await invoke(restored)
  await invoke(restored)
  for (const entry of f.manifest.files) assert.deepEqual(await readFile(join(restored, entry.path)), await readFile(join(f.source, entry.path)))
  const piece = f.manifest.files.find(entry => entry.category === 'offline').pieces[0]
  const original = await readFile(join(f.output, piece.file))
  await writeFile(join(f.output, piece.file), Buffer.alloc(piece.bytes, 100))
  await assert.rejects(invoke(join(f.root, 'ps-corrupt')), /checksum/)
  await writeFile(join(f.output, piece.file), original)
  await rm(join(f.output, piece.file))
  await assert.rejects(invoke(join(f.root, 'ps-missing')))
})

test('build bootstrap extracts pinned IDE and Windows resources and prepares the existing builder layout', async t => {
  const f = await fixture(t)
  const ide = join(f.root, 'ide')
  const basic = join(f.root, 'basic')
  await mkdir(ide)
  await mkdir(join(basic, 'pwsh'), { recursive: true })
  await writeFile(join(ide, 'manifest.json'), '{"fixture":true}\n')
  await writeFile(join(basic, 'pwsh/pwsh.exe'), 'portable shell')
  await mkdir(join(f.source, 'build-inputs'))
  await tar.c({ cwd: ide, file: join(f.source, 'build-inputs/ide-resources.tar.gz'), gzip: true }, ['manifest.json'])
  await tar.c({ cwd: ide, file: join(f.source, 'build-inputs/strata-runtime.tar.gz'), gzip: true }, ['manifest.json'])
  await tar.c({ cwd: basic, file: join(f.source, 'environment-components/windows-basic.tar.gz'), gzip: true }, ['pwsh'])
  await f.put('native-tools-metadata.json', '{}\n')
  await f.put('environment/media-verification.json', '{}\n')
  const files = ['build-inputs/ide-resources.tar.gz', 'build-inputs/strata-runtime.tar.gz', 'environment-components/windows-basic.tar.gz', 'native-tools-metadata.json', 'environment/media-verification.json']
    .map(path => ({ path, category: path.startsWith('build-inputs/') ? 'build-input' : 'offline' }))
  const manifest = await packReleaseAssets({ sourceRoot: f.source, outputDirectory: join(f.root, 'build-parts'), files, releaseVersion: '1.0.0', baseUrl })
  const app = join(f.root, 'app')
  await mkdir(app)
  await writeFile(join(app, 'package.json'), '{"version":"1.0.0"}\n')
  await bootstrapReleaseInputs({ appDirectory: app, manifest, inputsDirectory: f.source })
  assert.equal(await readFile(join(app, 'runtime/component-stage/windows-basic/pwsh/pwsh.exe'), 'utf8'), 'portable shell')
  assert.equal(await readFile(join(app, 'resources/ide/manifest.json'), 'utf8'), '{"fixture":true}\n')
  assert.equal(await readFile(join(app, 'resources/strata-runtime/manifest.json'), 'utf8'), '{"fixture":true}\n')
  assert.equal(await readFile(join(app, 'runtime/environment/media-verification.json'), 'utf8'), '{}\n')
  await bootstrapReleaseInputs({ appDirectory: app, manifest, inputsDirectory: f.source })
})

test('bootstrap retains a catalog with equivalent components in a different order and JSON key order', async t => {
  const f = await fixture(t)
  const path = join(f.root, 'catalog.json')
  const components = [
    { id: 'linux-basic', file: 'linux-basic.tar.gz', bytes: 12, sha256: 'a'.repeat(64) },
    { id: 'windows-basic', file: 'windows-basic.tar.gz', bytes: 34, sha256: 'b'.repeat(64) },
  ]
  await writeReleaseComponentCatalog(path, components)
  assert.deepEqual(JSON.parse(await readFile(path, 'utf8')), { version: 1, components })
  const reordered = components.toReversed().map(component => Object.fromEntries(Object.entries(component).reverse()))
  const retained = JSON.stringify({ components: reordered, version: 1 })
  await writeFile(path, retained)
  await writeReleaseComponentCatalog(path, components)
  assert.equal(await readFile(path, 'utf8'), retained)
})

test('bootstrap rejects a changed catalog checksum or version and preserves the existing bytes', async t => {
  const f = await fixture(t)
  const path = join(f.root, 'catalog.json')
  const descriptor = { id: 'windows-basic', file: 'windows-basic.tar.gz', bytes: 34, sha256: 'b'.repeat(64) }
  for (const existing of [
    { version: 1, components: [{ ...descriptor, sha256: 'c'.repeat(64) }] },
    { version: 2, components: [descriptor] },
  ]) {
    const retained = JSON.stringify(existing)
    await writeFile(path, retained)
    await assert.rejects(writeReleaseComponentCatalog(path, [descriptor]), /Existing component catalog differs/)
    assert.equal(await readFile(path, 'utf8'), retained)
  }
})

test('archive members cannot overwrite an existing differing build input', async t => {
  const f = await fixture(t)
  const archive = join(f.root, 'build.tar.gz')
  await tar.c({ cwd: f.source, file: archive, gzip: true }, ['说明.txt'])
  const destination = join(f.root, 'extract')
  await mkdir(destination)
  await writeFile(join(destination, '说明.txt'), 'retained')
  await assert.rejects(extractBuildInput(archive, destination), /checksum/)
  assert.equal(await readFile(join(destination, '说明.txt'), 'utf8'), 'retained')
})

test('archive traversal and duplicate members are rejected before extraction', async t => {
  const f = await fixture(t)
  const traversal = join(f.root, 'traversal.tar.gz')
  await tar.c({ cwd: f.source, file: traversal, gzip: true, prefix: '../escape' }, ['说明.txt'])
  await assert.rejects(extractBuildInput(traversal, join(f.root, 'traversal-output')), /Unsafe release path/)
  const duplicate = join(f.root, 'duplicate.tar.gz')
  await tar.c({ cwd: f.source, file: duplicate, gzip: true }, ['说明.txt', '说明.txt'])
  await assert.rejects(extractBuildInput(duplicate, join(f.root, 'duplicate-output')), /duplicate archive member/)
  await assert.rejects(readFile(join(f.root, 'escape/说明.txt')), { code: 'ENOENT' })
})
