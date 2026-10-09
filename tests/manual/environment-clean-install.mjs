/** Create and remove an owned clean Ubuntu test environment without changing host Windows features. */
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { promisify } from 'node:util'
import { randomUUID } from 'node:crypto'
import { createEnvironmentSetup, writeEnvironmentRecord } from '../../src/main/environment.ts'
import { createWindowsEnvironmentPlatform } from '../../src/main/environment-platform.ts'

assert.ok(process.argv.includes('--run'), 'Run explicitly with --run; this test creates only its own named WSL distribution')
const run = promisify(execFile)
const workspace = resolve('.')
const testRoot = resolve('.artifacts', `rainy-clean-environment-${randomUUID()}`)
const installRoot = join(testRoot, '安装 RainyAgent')
const userData = join(testRoot, 'userdata')
const settingsPath = join(userData, 'desktop.json')
const output = resolve('validation/environment/clean-linux-install.json')
await mkdir(installRoot, { recursive: true })
await mkdir(userData, { recursive: true })
let settings = {}
const platform = createWindowsEnvironmentPlatform({ installRoot, userData, mediaRoot: resolve('runtime/environment') })
const before = await platform.inspectSystem()
const report = { scope: 'Fresh Ubuntu import and Rainy Host startup offline; host already has WSL2, no Windows feature changes or desktop automation',
  testRoot, beforeDistributions: before.distributions.map(value => value.name), steps: [], cleanup: false }
let ownedName
const setup = createEnvironmentSetup({ installRoot, userData, mediaRoot: resolve('runtime/environment'),
  readDesktopSettings: async () => settings,
  writeDesktopSettings: async next => { settings = next; await writeEnvironmentRecord(settingsPath, settings) },
  onProgress: snapshot => {
    if (report.steps.at(-1)?.message !== snapshot.message) {
      report.steps.push({ status: snapshot.status, message: snapshot.message })
      process.stdout.write(JSON.stringify({ status: snapshot.status, message: snapshot.message }) + '\n')
    }
  },
})
try {
  assert.equal(before.wslInstalled, true)
  assert.equal(before.componentsEnabled, true)
  const first = await setup.inspect()
  assert.equal(first.status, 'needs-distro')
  ownedName = first.managedDistro
  assert.ok(!before.distributions.some(value => value.name === ownedName))
  const created = await setup.act({ type: 'create-managed-distro' })
  assert.equal(created.status, 'ready', `${created.code}: ${created.message}`)
  assert.equal(created.distro, ownedName)
  report.distro = ownedName
  const wsl = async (...args) => (await run('wsl.exe', ['--distribution', ownedName, ...args], { windowsHide: true, encoding: 'utf8', timeout: 240000, maxBuffer: 4 * 1024 * 1024 })).stdout.trim()
  const map = async value => wsl('--exec', 'wslpath', '-u', value)
  const installScript = await map(resolve('scripts/install-runtime.py'))
  const archive = await map(resolve('runtime/linux-runtime.tar.gz'))
  const metadata = await map(resolve('runtime/linux-runtime.json'))
  const installed = JSON.parse(await wsl('--exec', 'python3', installScript, archive, metadata))
  report.installedRuntime = installed
  assert.ok(installed.node.startsWith('/home/rainy/.rainy-agent/runtime/'))
  const driver = String.raw`
import json, selectors, socket, subprocess, sys, time, urllib.request, http.cookiejar
node, host = sys.argv[1:]
env = {'PATH':'/usr/bin:/bin','HOME':'/home/rainy','USER':'rainy','LANG':'C.UTF-8','RAINY_HOME':'/home/rainy/offline-acceptance','RAINY_CONFIGURE_DEEPSEEK':'0'}
process = subprocess.Popen([node, '--expose-internals', host], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
ready = None
selector = selectors.DefaultSelector()
selector.register(process.stdout, selectors.EVENT_READ)
deadline = time.monotonic() + 90
try:
  while time.monotonic() < deadline:
    if process.poll() is not None: raise RuntimeError('Host exited before ready: '+process.stderr.read()[-3000:])
    if not selector.select(1): continue
    line = process.stdout.readline()
    if line.startswith('RAINY_CONTROL '):
      item = json.loads(line[14:])
      if item.get('type') == 'fatal': raise RuntimeError(item.get('message'))
      if item.get('type') == 'ready': ready = item; break
  if ready is None: raise RuntimeError('Host readiness deadline exceeded')
  cookies = http.cookiejar.CookieJar()
  http = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies), urllib.request.ProxyHandler({}))
  response = http.open(ready['url'], timeout=20)
  assert response.status == 200
  origin = urllib.parse.urlsplit(ready['url'])
  ice = http.open(origin.scheme+'://'+origin.netloc+'/rainy/icesky/index.html?embed=rainy', timeout=20)
  assert ice.status == 200
  html = ice.read().decode('utf8')
  assert 'IceSky' in html or '冰天' in html
  interfaces = sorted(name for _, name in socket.if_nameindex())
  assert interfaces == ['lo'], interfaces
  print(json.dumps({'status':'passed','user':subprocess.check_output(['id','-un'],text=True).strip(),'interfaces':interfaces, 'hostReady':True,'mainHTTP':response.status,'iceSkyHTTP':ice.status,'node':subprocess.check_output([node,'--version'],text=True).strip()}))
finally:
  selector.close()
  if process.poll() is None:
    process.stdin.write(json.dumps({'type':'stop'}) + chr(10)); process.stdin.flush()
    process.stdin.close()
    try: process.wait(timeout=20)
    except subprocess.TimeoutExpired: process.kill(); process.wait(); raise
  if process.returncode != 0: raise RuntimeError('Host exit code '+str(process.returncode)+': '+process.stderr.read()[-3000:])
`
  const result = await wsl('--user', 'root', '--exec', 'unshare', '--net', '--', 'bash', '-c',
    'set -eu; ip link set lo up; exec runuser -u rainy -- python3 -c "$1" "$2" "$3"',
    'rainy-offline-acceptance', driver, installed.node, installed.host)
  report.host = JSON.parse(result)
  assert.equal(report.host.status, 'passed')
  const reopened = await setup.inspect()
  assert.equal(reopened.status, 'ready')
  assert.equal(reopened.distro, ownedName)
  report.reopened = true
} catch (error) {
  report.failure = error instanceof Error && 'stderr' in error ? String(error.stderr).slice(-4000) : error instanceof Error ? error.message : String(error)
  process.exitCode = 1
} finally {
  if (ownedName !== undefined) {
    const system = await platform.inspectSystem()
    const registered = system.distributions.find(value => value.name === ownedName)
    if (registered) {
      const owner = JSON.parse(await readFile(join(installRoot, 'runtime/wsl/owner.json'), 'utf8'))
      const expectedPath = resolve(installRoot, 'runtime/wsl/data')
      const actualPath = resolve(registered.basePath.replace(/^\\\\\?\\/, ''))
      assert.equal(owner.installRoot, installRoot)
      assert.equal(owner.distro, ownedName)
      assert.equal(actualPath.toLowerCase(), expectedPath.toLowerCase())
      assert.ok(installRoot.startsWith(`${workspace}\\.artifacts\\rainy-clean-environment-`))
      await run('wsl.exe', ['--terminate', ownedName], { windowsHide: true, timeout: 60000 })
      await run('wsl.exe', ['--unregister', ownedName], { windowsHide: true, timeout: 60000 })
    }
    const after = await platform.inspectSystem()
    assert.deepEqual(after.distributions.map(value => value.name).sort(), before.distributions.map(value => value.name).sort())
    report.cleanup = true
  }
  await mkdir(resolve(output, '..'), { recursive: true })
  await writeFile(output, `${JSON.stringify(report, null, 2)}\n`)
  process.stdout.write(JSON.stringify({ status: report.failure ? 'failed' : 'passed', cleanup: report.cleanup, output, failure: report.failure }) + '\n')
}
