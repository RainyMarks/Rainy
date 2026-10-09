/** Build and run a private real-Electron ASAR installer fixture; never use the installed application directory. */
import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { spawn } from 'node:child_process'
import { createRequire } from 'node:module'
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, isAbsolute, join, relative, resolve } from 'node:path'

const mode = process.argv[2] ?? 'after'
assert(['before', 'after'].includes(mode))
const app = resolve(import.meta.dirname, '../..')
const requireApp = createRequire(join(app, 'package.json'))
const requireBuilder = createRequire(requireApp.resolve('electron-builder/package.json'))
const requireAppBuilder = createRequire(requireBuilder.resolve('app-builder-lib/package.json'))
const asar = requireAppBuilder('@electron/asar')
const { build } = requireApp('esbuild')
const { c } = requireApp('tar')
const electron = requireApp('electron')
const root = await mkdtemp(join(tmpdir(), 'rainy-electron-asar-'))
const output = process.argv[3] ? resolve(process.argv[3]) : join(app, 'validation/native-tools-0.3.0/electron-asar-regression')
await mkdir(output, { recursive: true })
const hash = bytes => createHash('sha256').update(bytes).digest('hex')
let child
let closed
let childResult
let stdout = ''
let stderr = ''
let deadline
let timedOut = false
try {
  const installRoot = join(root, 'installed')
  const carrier = join(root, 'carrier')
  await mkdir(installRoot)
  await mkdir(join(carrier, 'setup'), { recursive: true })
  await writeFile(join(carrier, 'package.json'), JSON.stringify({ name: 'rainy-private-asar-test', main: 'main.cjs' }))
  await writeFile(join(carrier, 'setup/toolpack.html'), 'private carrier setup asset\n')
  await build({ entryPoints: [join(app, 'tests/manual/fixtures/toolpack-electron.ts')], outfile: join(carrier, 'main.cjs'),
    bundle: true, platform: 'node', target: 'node22', format: 'cjs', external: ['electron'] })
  async function packageFixture(version) {
    const source = join(root, version, 'source')
    const mediaDirectory = join(root, version, 'media')
    const archiveSource = join(root, version, 'asar-source')
    await mkdir(join(source, 'tools/bruno/resources/app.asar.unpacked'), { recursive: true })
    await mkdir(mediaDirectory, { recursive: true })
    await mkdir(archiveSource, { recursive: true })
    await writeFile(join(archiveSource, 'probe.txt'), `third-party archive ${version}\n`)
    await asar.createPackage(archiveSource, join(source, 'tools/bruno/resources/app.asar'))
    await writeFile(join(source, 'tools/bruno/resources/app.asar.unpacked/native.txt'), `unpacked ${version}\n`)
    await writeFile(join(source, 'tools/manifest.json'), JSON.stringify({ version: 1, tools: [] }))
    const paths = ['tools/bruno/resources/app.asar', 'tools/bruno/resources/app.asar.unpacked/native.txt', 'tools/manifest.json']
    const files = await Promise.all(paths.map(async path => { const bytes = await readFile(join(source, path)); return { path, bytes: bytes.length, sha256: hash(bytes) } }))
    const units = [{ path: 'tools/bruno', kind: 'directory', preserve: [] }, { path: 'tools/manifest.json', kind: 'file', preserve: [] }]
    const id = hash(JSON.stringify({ files, units }))
    const chunks = []
    for await (const chunk of c({ cwd: source, gzip: true, portable: true, noDirRecurse: true, mtime: new Date(0) }, paths)) chunks.push(Buffer.from(chunk))
    const archive = Buffer.concat(chunks)
    const volume = `native-tools-${id.slice(0, 16)}.tar.gz.001`
    await writeFile(join(mediaDirectory, volume), archive)
    const metadata = { version: 1, id, format: 'tar.gz', volumeSize: 65536, unpackedBytes: files.reduce((sum, file) => sum + file.bytes, 0), files, units,
      volumes: [{ file: volume, bytes: archive.length, sha256: hash(archive) }] }
    const metadataPath = join(root, version, 'metadata.json')
    await writeFile(metadataPath, JSON.stringify(metadata))
    return { options: { installRoot, mediaDirectory, metadataPath }, metadata }
  }
  const first = await packageFixture('first')
  const next = await packageFixture('next')
  const config = join(root, 'fixture.json')
  await writeFile(config, JSON.stringify({ root, installRoot, first: first.options, next: next.options,
    firstArchiveSha256: first.metadata.files[0].sha256, nextArchiveSha256: next.metadata.files[0].sha256, nextPackId: next.metadata.id }))
  const carrierArchive = join(root, 'carrier.asar')
  await asar.createPackage(carrier, carrierArchive)
  const environment = { ...process.env }
  delete environment.ELECTRON_RUN_AS_NODE
  delete environment.ELECTRON_NO_ASAR
  child = spawn(electron, [carrierArchive, config, `--user-data-dir=${join(root, 'electron-user-data')}`],
    { windowsHide: true, env: environment, stdio: ['ignore', 'pipe', 'pipe'] })
  closed = new Promise((accept, reject) => {
    child.once('error', reject)
    child.once('close', (code, signal) => { childResult = { code, signal }; accept(childResult) })
  })
  child.stdout.on('data', chunk => { stdout = (stdout + String(chunk)).slice(-200000) })
  child.stderr.on('data', chunk => { stderr = (stderr + String(chunk)).slice(-40000) })
  deadline = setTimeout(() => { timedOut = true; child.kill('SIGKILL') }, 45000)
  const exit = await closed
  clearTimeout(deadline)
  const raw = stdout.split(/\r?\n/).find(line => line.startsWith('RAINY_ASAR_TEST '))
  const result = raw ? JSON.parse(raw.slice('RAINY_ASAR_TEST '.length)) : undefined
  const report = { mode, command: 'node tests/manual/toolpack-electron.mjs ' + mode,
    electronExecutable: electron, carrierArchiveSha256: hash(await readFile(carrierArchive)), exit, timedOut, result, stderr,
    scope: 'Private temporary Electron app.asar and tool payload only; the real installed stage, journals, tools, and settings were not modified.' }
  await writeFile(join(output, `${mode}.json`), JSON.stringify(report, null, 2) + '\n')
  assert.equal(timedOut, false)
  assert(result, stdout + stderr)
  if (mode === 'before') {
    assert.equal(exit.code, 1)
    assert.equal(result.passed, false)
    assert.equal(result.phase, 'raw missing archive stat')
    assert.match(result.error, /Invalid package/)
  } else { assert.equal(exit.code, 0, result.error); assert.equal(result.passed, true, result.error) }
  console.log(JSON.stringify({ ...report, report: join(output, `${mode}.json`) }))
} finally {
  clearTimeout(deadline)
  if (child && !childResult) { child.kill('SIGKILL'); await closed }
  const childPath = relative(tmpdir(), root)
  assert(childPath && !childPath.startsWith('..') && !isAbsolute(childPath), 'Fixture cleanup escaped the temporary directory')
  await rm(root, { recursive: true, force: true })
}
