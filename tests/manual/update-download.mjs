/** Manually verify a built Windows release through real Electron downloads; never execute the installer. */
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { createRequire } from 'node:module'
import { tmpdir } from 'node:os'
import { basename, isAbsolute, join, relative, resolve, sep } from 'node:path'
import { parseArgs } from 'node:util'

const { values } = parseArgs({ options: { artifacts: { type: 'string' }, report: { type: 'string' } } })
assert.equal(process.platform, 'win32', 'The NSIS download fixture requires Windows')
assert(values.artifacts, 'Usage: node tests/manual/update-download.mjs --artifacts <release-directory> [--report <file.json>]')
const artifacts = resolve(values.artifacts)
const appDirectory = resolve(import.meta.dirname, '../..')
const requireApp = createRequire(join(appDirectory, 'package.json'))
const { load, dump } = requireApp('js-yaml')
const latestBytes = await readFile(join(artifacts, 'latest.yml'))
const latest = load(latestBytes.toString('utf8'))
assert(latest && typeof latest === 'object' && typeof latest.version === 'string')
assert.match(latest.version, /^[1-9]\d*\.\d+\.\d+$/u, 'Use a stable release newer than the private 0.0.0 carrier')
assert(Array.isArray(latest.files))
const installers = latest.files.filter(file => typeof file.url === 'string' && file.url.endsWith('.exe'))
assert.equal(installers.length, 1, 'The feed must name one Windows installer')
const installer = installers[0]
assert.equal(basename(installer.url), installer.url)
assert(!/[\\/:]/u.test(installer.url), 'Installer URL must be a local release filename')
assert.equal(typeof installer.sha512, 'string')
const installerPath = join(artifacts, installer.url)
const installerStat = await stat(installerPath)
assert(installerStat.isFile())
assert.equal(installer.size, installerStat.size)

async function digest(path) {
  const sha256 = createHash('sha256')
  const sha512 = createHash('sha512')
  let bytes = 0
  for await (const chunk of createReadStream(path)) {
    sha256.update(chunk)
    sha512.update(chunk)
    bytes += chunk.length
  }
  return { bytes, sha256: sha256.digest('hex'), sha512: sha512.digest('base64') }
}

const original = await digest(installerPath)
assert.equal(original.sha512, installer.sha512, 'latest.yml must match the supplied installer')
const blockmap = await digest(`${installerPath}.blockmap`)
const corruptBody = Buffer.from('Private checksum rejection fixture; this is not executable.\n')
const corruptSha512 = createHash('sha512').update('different bytes').digest('base64')
const corruptLatest = Buffer.from(dump({ ...latest, path: 'checksum-rejected.exe', sha512: corruptSha512,
  files: [{ url: 'checksum-rejected.exe', sha512: corruptSha512, size: corruptBody.length }] }))
const requests = []
const server = createServer((request, response) => {
  const pathname = new URL(request.url, 'http://127.0.0.1').pathname
  requests.push({ method: request.method, path: pathname })
  response.setHeader('Cache-Control', 'no-store')
  if (request.method !== 'GET') { response.writeHead(405).end(); return }
  if (pathname === '/release/latest.yml' || pathname === '/corrupt/latest.yml') {
    const bytes = pathname.startsWith('/corrupt/') ? corruptLatest : latestBytes
    response.writeHead(200, { 'Content-Type': 'text/yaml', 'Content-Length': bytes.length }).end(bytes)
  } else if (pathname === `/release/${encodeURIComponent(installer.url)}` || pathname === `/release/${installer.url}`) {
    response.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': original.bytes })
    const stream = createReadStream(installerPath)
    stream.once('error', error => response.destroy(error))
    response.once('close', () => stream.destroy())
    stream.pipe(response)
  } else if (pathname === '/corrupt/checksum-rejected.exe') {
    response.writeHead(200, { 'Content-Type': 'application/octet-stream', 'Content-Length': corruptBody.length }).end(corruptBody)
  } else response.writeHead(404).end()
})
const root = await mkdtemp(join(tmpdir(), 'rainy-update-download-'))
const report = { version: latest.version, installer: { file: installer.url, ...original }, blockmap,
  electronVersion: requireApp('electron/package.json').version,
  updaterVersion: requireApp('electron-updater/package.json').version, scenarios: [], passed: false,
  scope: 'Loopback download and checksum verification in private Electron carriers. No installer execution, application replacement, or installed-user-data access.' }

async function runScenario(scenario, feedUrl) {
  const directory = join(root, scenario)
  const carrier = join(directory, 'carrier')
  for (const path of [carrier, ...['app-data', 'user-data', 'session-data', 'logs', 'local-app-data', 'temp'].map(name => join(directory, name))]) {
    await mkdir(path, { recursive: true })
  }
  const currentVersion = scenario === 'current' ? latest.version : '0.0.0'
  const reportPath = join(directory, 'result.json')
  await writeFile(join(carrier, 'package.json'), JSON.stringify({ name: 'rainy-update-download-fixture', version: currentVersion, main: 'main.cjs' }))
  await writeFile(join(carrier, 'main.cjs'), `require(${JSON.stringify(join(import.meta.dirname, 'fixtures/update-download-electron.cjs'))})\n`)
  await writeFile(join(carrier, 'dev-app-update.yml'), dump({ provider: 'generic', url: feedUrl, updaterCacheDirName: 'rainy-update-fixture' }))
  await writeFile(join(carrier, 'fixture.json'), JSON.stringify({ root: directory, currentVersion, reportPath, scenario, feedUrl,
    updaterEntry: requireApp.resolve('electron-updater') }))
  const env = { ...process.env, LOCALAPPDATA: join(directory, 'local-app-data'), APPDATA: join(directory, 'app-data'),
    TEMP: join(directory, 'temp'), TMP: join(directory, 'temp') }
  delete env.ELECTRON_RUN_AS_NODE
  delete env.ELECTRON_NO_ASAR
  let stderr = ''
  let timedOut = false
  const start = requests.length
  const child = spawn(requireApp('electron'), [carrier, `--user-data-dir=${join(directory, 'user-data')}`],
    { windowsHide: true, env, stdio: ['ignore', 'ignore', 'pipe'] })
  child.stderr.on('data', bytes => { stderr = (stderr + String(bytes)).slice(-20000) })
  const closed = new Promise((accept, reject) => {
    child.once('error', reject)
    child.once('close', (code, signal) => accept({ code, signal }))
  })
  let killerClosed
  const timeout = setTimeout(() => {
    timedOut = true
    if (!child.pid) return
    const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' })
    killerClosed = new Promise(accept => { killer.once('error', accept); killer.once('close', accept) })
  }, 180000)
  let exit
  try { exit = await closed } finally { clearTimeout(timeout); if (killerClosed) await killerClosed }
  let result
  try { result = JSON.parse(await readFile(reportPath, 'utf8')) } catch (error) {
    throw new Error(`Electron ${scenario} produced no report: ${JSON.stringify(exit)}; ${stderr}`, { cause: error })
  }
  const evidence = { ...result, exit, timedOut, requests: requests.slice(start), stderr }
  report.scenarios.push(evidence)
  assert.equal(timedOut, false)
  assert.equal(exit.code, 0, result.error)
  assert.equal(result.passed, true, result.error)
  if (scenario === 'current') {
    assert.deepEqual(evidence.requests.map(request => request.path), ['/release/latest.yml'])
  } else if (scenario === 'available') {
    const downloaded = resolve(directory, result.downloaded)
    const inside = relative(directory, downloaded)
    assert(inside && inside !== '..' && !inside.startsWith(`..${sep}`) && !isAbsolute(inside))
    evidence.download = await digest(downloaded)
    assert.deepEqual(evidence.download, original)
    assert(evidence.requests.some(request => request.path.endsWith('.exe')))
  } else assert.equal(result.rejectedChecksum, true)
  console.log(JSON.stringify({ scenario, passed: true, requests: evidence.requests, bytes: evidence.download?.bytes }))
}

try {
  await new Promise((accept, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', accept) })
  const address = server.address()
  assert(address && typeof address !== 'string')
  const baseUrl = `http://127.0.0.1:${address.port}`
  await runScenario('current', `${baseUrl}/release`)
  await runScenario('available', `${baseUrl}/release`)
  await runScenario('corrupt', `${baseUrl}/corrupt`)
  report.passed = true
} catch (error) {
  report.error = error.stack ?? String(error)
  throw error
} finally {
  server.closeAllConnections()
  await new Promise(accept => server.close(accept))
  try {
    if (values.report) await writeFile(resolve(values.report), JSON.stringify(report, null, 2) + '\n')
  } finally {
    const inside = relative(tmpdir(), root)
    assert(inside && inside !== '..' && !inside.startsWith(`..${sep}`) && !isAbsolute(inside), 'Fixture cleanup escaped the temporary directory')
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 })
  }
}
console.log(JSON.stringify({ passed: true, version: report.version, installer: report.installer, scenarios: report.scenarios.length }))
