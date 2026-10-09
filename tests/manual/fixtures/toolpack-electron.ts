/** Real Electron main-process checks for opaque native-tool ASAR files. */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { lstat } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { app } from 'electron'
import type * as FileSystem from 'node:fs'
import { createNativeToolPackInstaller } from '../../../src/main/toolpack.ts'
import { toolPackHash, toolPackStat, toolPackTree } from '../../../src/main/toolpack-files.ts'
import { nativeToolPackPlatform } from '../../../src/main/toolpack-platform.ts'
import type { InstallNativeToolPackOptions } from '../../../src/main/toolpack-format.ts'
import { NativeToolsLibrary } from '../../../src/main/native-tools.ts'
import { serveNativeTool } from '../../../src/main/native-tool-web.ts'

interface Fixture {
  readonly root: string
  readonly installRoot: string
  readonly first: InstallNativeToolPackOptions
  readonly next: InstallNativeToolPackOptions
  readonly firstArchiveSha256: string
  readonly nextArchiveSha256: string
  readonly nextPackId: string
}

const original = createRequire(process.execPath)('original-fs') as typeof FileSystem
const fixture: Fixture = JSON.parse(readFileSync(process.argv[2], 'utf8')) as Fixture
const carrierAsset = join(__dirname, 'setup/toolpack.html')
const beforeNoAsar = process.noAsar
const checks: object[] = []
let phase = 'starting'

async function run(): Promise<void> {
  assert(process.versions.electron, 'This fixture must use real Electron main mode')
  assert.notEqual(process.noAsar, true)
  assert.equal(readFileSync(carrierAsset, 'utf8'), 'private carrier setup asset\n')
  const missingArchive = join(fixture.installRoot, 'tools/bruno/resources/app.asar')
  let patchedFailure = ''
  try { await lstat(missingArchive) }
  catch (error) { patchedFailure = error instanceof Error ? error.message : String(error) }
  assert.match(patchedFailure, /Invalid package/)
  checks.push({ name: 'ordinary Electron fs reproduces missing-ASAR Invalid package', passed: true, error: patchedFailure })
  phase = 'raw missing archive stat'
  assert.equal(await toolPackStat(missingArchive), undefined)
  const install = createNativeToolPackInstaller({ ...nativeToolPackPlatform,
    availableBytes: async () => 1024 ** 4, assertNotBusy: async () => {} })
  let carrierAssetReads = 0
  let carrierAssetError: unknown
  const readCarrier = (): void => {
    try {
      assert.equal(readFileSync(carrierAsset, 'utf8'), 'private carrier setup asset\n')
      assert.equal(process.noAsar, beforeNoAsar)
      carrierAssetReads++
    } catch (error) { carrierAssetError = error }
  }
  phase = 'install first ASAR payload'
  const first = await install({ ...fixture.first, onProgress: readCarrier })
  const installedArchive = join(fixture.installRoot, 'tools/bruno/resources/app.asar')
  assert((await toolPackStat(installedArchive))?.isFile())
  assert.equal(await toolPackHash(installedArchive), fixture.firstArchiveSha256)
  const tree = await toolPackTree(join(fixture.installRoot, 'tools/bruno'))
  assert.equal(tree.filter(entry => entry.path === installedArchive).length, 1)
  assert(!tree.some(entry => entry.path.startsWith(installedArchive + '/')))
  assert.equal(await original.promises.readFile(join(fixture.installRoot, 'tools/bruno/resources/app.asar.unpacked/native.txt'), 'utf8'), 'unpacked first\n')
  const userArchive = join(fixture.installRoot, 'tools/bruno/personal.asar')
  await original.promises.writeFile(userArchive, 'personal opaque archive bytes')
  checks.push({ name: 'ASAR installs and hashes as one opaque file beside unpacked files', passed: true, first })

  phase = 'cancel ASAR upgrade after a verified entry'
  const cancellation = new AbortController()
  await assert.rejects(install({ ...fixture.next, signal: cancellation.signal, onProgress(update) {
    readCarrier()
    if (update.phase === 'extracting' && update.currentPath === 'tools/bruno/resources/app.asar') cancellation.abort()
  } }), (error: unknown) => error instanceof Error && 'code' in error && error.code === 'cancelled')
  assert.equal(await toolPackHash(installedArchive), fixture.firstArchiveSha256)
  assert.equal(await original.promises.readFile(userArchive, 'utf8'), 'personal opaque archive bytes')
  const stagedArchive = join(fixture.installRoot, '.rainy-toolpack/stage', fixture.nextPackId, 'tools/bruno/resources/app.asar')
  assert((await toolPackStat(stagedArchive))?.isFile())
  assert.equal(await toolPackHash(stagedArchive), fixture.nextArchiveSha256)
  checks.push({ name: 'cancellation retains the old payload and verified staged ASAR', passed: true })

  phase = 'retry ASAR upgrade'
  const upgraded = await install({ ...fixture.next, onProgress: readCarrier })
  assert(upgraded.reusedFiles > 0)
  assert.equal(await toolPackHash(installedArchive), fixture.nextArchiveSha256)
  assert.equal(await original.promises.readFile(userArchive, 'utf8'), 'personal opaque archive bytes')
  assert.equal(await original.promises.readFile(join(fixture.installRoot, 'tools/bruno/resources/app.asar.unpacked/native.txt'), 'utf8'), 'unpacked next\n')
  assert.equal(await toolPackHash(join(upgraded.backupDirectory, 'tools/bruno/resources/app.asar')), fixture.firstArchiveSha256)
  assert.equal(await original.promises.readFile(join(upgraded.backupDirectory, 'tools/bruno/personal.asar'), 'utf8'), 'personal opaque archive bytes')
  assert.equal((JSON.parse(await original.promises.readFile(join(fixture.installRoot, '.rainy-toolpack/journal.json'), 'utf8')) as { phase: string }).phase, 'committed')
  checks.push({ name: 'retry reuses raw ASAR and preserves personal ASAR plus backup bytes', passed: true, upgraded })

  phase = 'catalog probes opaque third-party ASAR dependencies'
  const nativeRoot = join(fixture.installRoot, 'tools/bruno')
  const executable = join(nativeRoot, 'Bruno.exe')
  const webpage = join(nativeRoot, 'index.html')
  await original.promises.writeFile(executable, 'private non-executed fixture')
  await original.promises.writeFile(webpage, '<!doctype html><title>private tool fixture</title>')
  await original.promises.writeFile(join(fixture.installRoot, 'tools/manifest.json'), JSON.stringify({ version: 1, tools: [
    { id: 'bruno', category: 'web', name: 'Bruno fixture', version: 'fixture', roots: ['tools/bruno'],
      entry: { kind: 'gui', path: 'tools/bruno/Bruno.exe', cwd: 'tools/bruno',
        requiredFiles: ['tools/bruno/resources/app.asar', 'tools/bruno/personal.asar'] } },
  ] }))
  const library = new NativeToolsLibrary({ installRoot: fixture.installRoot, userData: join(fixture.root, 'catalog-user'),
    start: async () => { throw new Error('Catalog inspection must not launch a program') } })
  const catalog = await library.listTools()
  assert.equal(catalog.tools.length, 1)
  assert.equal(catalog.tools[0]?.status, 'ready', JSON.stringify(catalog.tools[0]))
  assert.deepEqual(catalog.tools[0]?.missing, [])
  assert.equal(await toolPackHash(installedArchive), fixture.nextArchiveSha256)
  assert.equal(await original.promises.readFile(userArchive, 'utf8'), 'personal opaque archive bytes')
  checks.push({ name: 'native catalog probes valid and arbitrary ASAR files as opaque bytes without changing them', passed: true })

  phase = 'offline webpage streams opaque third-party ASAR bytes'
  const page = await serveNativeTool({ id: 'bruno', name: 'Bruno fixture', kind: 'web',
    target: webpage, executable: webpage, cwd: nativeRoot, args: [], roots: [nativeRoot], userData: nativeRoot })
  try {
    for (const [path, expected] of [
      ['resources/app.asar', await original.promises.readFile(installedArchive)],
      ['personal.asar', await original.promises.readFile(userArchive)],
    ] as const) {
      const response = await fetch(new URL(path, page.url))
      assert.equal(response.status, 200)
      assert.deepEqual(Buffer.from(await response.arrayBuffer()), expected)
    }
    assert.equal((await fetch(new URL('resources/app.asar/probe.txt', page.url))).status, 404)
  } finally { await page.close() }
  assert.equal(await toolPackHash(installedArchive), fixture.nextArchiveSha256)
  assert.equal(await original.promises.readFile(userArchive, 'utf8'), 'personal opaque archive bytes')
  checks.push({ name: 'offline webpage serves complete raw archives and refuses virtual archive traversal without changing bytes', passed: true })

  assert.equal(carrierAssetError, undefined)
  assert(carrierAssetReads > 0)
  assert.equal(process.noAsar, beforeNoAsar)
  assert.equal(readFileSync(carrierAsset, 'utf8'), 'private carrier setup asset\n')
  checks.push({ name: 'carrier ASAR asset stays readable during installation without global noAsar changes', passed: true, reads: carrierAssetReads })
}

void app.whenReady().then(run).then(() => {
  console.log('RAINY_ASAR_TEST ' + JSON.stringify({ passed: true, electron: process.versions.electron,
    root: fixture.root, carrier: dirname(carrierAsset), checks }))
  app.exit(0)
}, (error: unknown) => {
  console.log('RAINY_ASAR_TEST ' + JSON.stringify({ passed: false, electron: process.versions.electron,
    root: fixture.root, phase, checks, error: error instanceof Error ? error.stack : String(error) }))
  app.exit(1)
})
