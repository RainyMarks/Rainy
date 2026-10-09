/** Built-in-Node acceptance for an installed or relocated Host in a loopback-only network namespace. */
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { mkdtemp, mkdir, readFile, readlink, realpath } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { createInterface } from 'node:readline'
import { setTimeout as delay } from 'node:timers/promises'

const checks = []
const startedAt = new Date().toISOString()
const started = performance.now()
let child
let lines
let closed
let childResult
let childError
let diagnostic = ''
let cookie = ''
let origin
let home
let project
let hostPath
let hostSha256
let network
let orderlyShutdown = false
let failure
const deadlineMs = 60000
const requestMs = 15000
const digest = bytes => createHash('sha256').update(bytes).digest('hex')
const secrets = new Set()

async function bounded(promise, milliseconds, description) {
  let timer
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(description)), milliseconds)
    })])
  } finally { clearTimeout(timer) }
}

async function stopHost() {
  if (!child || childResult) return childResult
  if (child.stdin.writable) child.stdin.end('{"type":"stop"}\n')
  try { return await bounded(closed, 15000, 'Host did not close after stop and stdin EOF') }
  catch (error) {
    child.kill('SIGTERM')
    try { await bounded(closed, 5000, 'Owned Host did not close after SIGTERM') }
    catch (_termDeadline) {
      child.kill('SIGKILL')
      await bounded(closed, 5000, 'Owned Host did not close after SIGKILL')
    }
    throw error
  }
}

async function request(path, body) {
  const response = await fetch(new URL(path, origin), {
    method: body === undefined ? 'GET' : 'POST',
    headers: { cookie, origin, ...(body === undefined ? {} : { 'content-type': 'application/json' }) },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    redirect: 'manual', signal: AbortSignal.timeout(requestMs),
  })
  if (response.status !== 200) assert.fail(`${path} returned HTTP ${response.status}: ${(await response.text()).slice(0, 1000)}`)
  return response
}

async function ide(body) {
  const result = await (await request('/rainy/ide', body)).json()
  assert.equal(result.ok, true, `${body.op}: ${JSON.stringify(result.error)}`)
  return result.value
}

async function until(read, description, milliseconds = deadlineMs) {
  const deadline = performance.now() + milliseconds
  while (performance.now() < deadline) {
    if (childResult) throw new Error(`Host closed while waiting for ${description}: ${JSON.stringify(childResult)}`)
    if (childError) throw childError
    const value = await read()
    if (value) return value
    await delay(100)
  }
  throw new Error(`Timed out waiting for ${description}`)
}

async function run(workspaceId, configuration) {
  const launched = await ide({ op: 'run.start', workspaceId, configuration })
  const finished = await until(async () => {
    const status = await ide({ op: 'execution.status', workspaceId })
    const current = status.runs.find(entry => entry.id === launched.id)
    if (current?.phase === 'failed') throw new Error(`${configuration.name}: ${current.error}`)
    return current?.phase === 'exited' ? current : undefined
  }, `${configuration.language} run completion`)
  assert.equal(finished.exit.exitCode, 0, JSON.stringify(finished.exit))
  assert.equal(finished.exit.stopped, false)
  const result = await ide({ op: 'execution.poll', workspaceId, cursor: 0 })
  assert.equal(result.truncated, false)
  const output = result.events.filter(event => event.kind === 'output' && event.operationId === launched.id)
    .map(event => event.text).join('')
  assert.match(output, /(?:^|\r?\n)42\r?\n/, `${configuration.name} did not print 42: ${output}`)
  return { id: launched.id, language: configuration.language, argv: launched.spec.launch.argv,
    build: launched.spec.build, exit: finished.exit, output }
}

try {
  assert.equal(process.platform, 'linux', 'Run this fixture inside the installed Linux runtime')
  assert(process.argv[2], 'Pass the installed or relocated absolute app/lib/host.js path')
  assert(process.argv[2].startsWith('/'), 'Host path must be absolute')
  hostPath = await realpath(process.argv[2])
  hostSha256 = digest(await readFile(hostPath))
  const interfaces = (await readFile('/proc/net/dev', 'utf8')).split('\n').slice(2)
    .filter(line => line.includes(':')).map(line => line.split(':')[0].trim()).sort()
  assert.deepEqual(interfaces, ['lo'], 'The network namespace must expose only loopback')
  const routes = await readFile('/proc/net/route', 'utf8')
  assert(!routes.split('\n').slice(1).some(line => /^\S+\s+00000000\s/.test(line)), 'Offline namespace has a default route')
  network = { namespace: await readlink('/proc/self/ns/net'), interfaces, defaultIpv4Route: false }
  checks.push({ name: 'loopback-only network namespace', passed: true, ...network })
  home = await mkdtemp(join(tmpdir(), 'rainy-release-offline-'))
  project = join(home, 'project')
  await mkdir(project)
  const environment = { ...process.env }
  for (const name of Object.keys(environment)) {
    if (/(?:KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL)/i.test(name) || /^(?:HTTP_PROXY|HTTPS_PROXY|ALL_PROXY)$/i.test(name)
      || name.startsWith('RAINY_') || name.startsWith('DSH_')) delete environment[name]
  }
  Object.assign(environment, { RAINY_HOME: home, DSH_HOME: home, RAINY_CONFIGURE_DEEPSEEK: '0', RAINY_DETACHED: '0',
    DSH_TELEMETRY_DISABLED: '1', NO_PROXY: '127.0.0.1,localhost' })
  child = spawn(process.execPath, ['--expose-internals', hostPath], { cwd: project, env: environment, stdio: ['pipe', 'pipe', 'pipe'] })
  closed = new Promise(accept => child.once('close', (code, signal) => { childResult = { code, signal }; accept(childResult) }))
  child.on('error', error => { childError = error })
  child.stdin.on('error', error => { if (error.code !== 'EPIPE') childError = error })
  child.stderr.on('data', data => { diagnostic = (diagnostic + String(data)).slice(-8000) })
  lines = createInterface({ input: child.stdout })
  let readyValue
  let startupError
  lines.on('line', line => {
    if (!line.startsWith('RAINY_CONTROL ')) return
    try {
      const event = JSON.parse(line.slice(14))
      if (event.type === 'ready') readyValue = event
      else if (event.type === 'fatal') startupError = new Error(event.message)
    } catch (error) { startupError = error }
  })
  const ready = await until(async () => { if (startupError) throw startupError; return readyValue }, 'Host readiness')
  const readyUrl = new URL(ready.url)
  for (const value of readyUrl.searchParams.values()) secrets.add(value)
  assert.equal(readyUrl.protocol, 'http:')
  assert.equal(readyUrl.hostname, '127.0.0.1')
  origin = readyUrl.origin
  const authenticated = await fetch(readyUrl, { redirect: 'manual', signal: AbortSignal.timeout(requestMs) })
  assert([200, 302, 303].includes(authenticated.status), `Authentication returned ${authenticated.status}`)
  cookie = authenticated.headers.getSetCookie().map(value => value.split(';')[0]).join('; ')
  secrets.add(cookie)
  for (const item of cookie.split('; ')) secrets.add(item.slice(item.indexOf('=') + 1))
  assert(cookie, 'Authentication did not issue a session cookie')
  await authenticated.arrayBuffer()
  const html = await (await request('/')).text()
  assert(html.includes('__RAINY_AGENT__') && html.includes('/rainy/panel.js'))
  const asset = Buffer.from(await (await request('/rainy/editor/editor.js')).arrayBuffer())
  assert(asset.length > 0)
  const control = await (await request('/rainy/control')).json()
  assert.deepEqual(control.tools, ['read', 'write', 'edit', 'bash'])
  assert.equal(control.models.length, 0)
  checks.push({ name: 'authenticated Host, HTML, editor asset and unchanged Agent roster', passed: true,
    startupMs: performance.now() - started, htmlBytes: Buffer.byteLength(html), editorBytes: asset.length,
    editorSha256: digest(asset), defaultTools: control.tools, configuredModels: control.models.length })

  const workspace = await ide({ op: 'workspaces.open', path: project })
  const workspaceId = workspace.workspaceId
  assert.equal(workspace.path, project)
  const created = await ide({ op: 'files.create', workspaceId, path: 'main.py', content: '\uFEFFvalue = 40\r\nprint(value + 1)\r\n' })
  assert.equal(created.bom, true)
  assert.equal(created.eol, 'crlf')
  await ide({ op: 'files.save', workspaceId, path: 'main.py', content: 'value = 41\nprint(value + 1)\n', expectedVersion: created.version })
  const reread = await ide({ op: 'files.read', workspaceId, path: 'main.py' })
  assert.equal(reread.bom, true)
  assert.equal(reread.eol, 'crlf')
  assert.equal(reread.content, 'value = 41\r\nprint(value + 1)\r\n')
  const raw = await readFile(join(project, 'main.py'))
  assert.equal(raw.toString('utf8'), '\uFEFFvalue = 41\r\nprint(value + 1)\r\n')
  checks.push({ name: 'IDE create/save/re-read preserves BOM and CRLF', passed: true, path: 'main.py', bytes: raw.length, sha256: digest(raw) })

  const python = await run(workspaceId, { name: 'Release Python', language: 'python', program: 'main.py', terminal: false })
  checks.push({ name: 'Python run prints 42', passed: true, ...python })
  await ide({ op: 'files.create', workspaceId, path: 'main.cpp', content: '#include <iostream>\nint main() { std::cout << 42 << std::endl; }\n' })
  const native = await run(workspaceId, { name: 'Release C++', language: 'cpp', program: 'main.cpp', terminal: false,
    build: { kind: 'single-file', flags: [] } })
  assert.equal(native.build.length, 1)
  assert(native.build[0].argv.includes('-g'))
  checks.push({ name: 'C++ single-file build and run prints 42', passed: true, ...native })

  await ide({ op: 'files.create', workspaceId, path: 'debug.py', content: 'value = 41\nprint(value + 1)\n' })
  const debug = await ide({ op: 'debug.start', workspaceId, configuration: { name: 'Release Python debug', language: 'python',
    program: 'debug.py', terminal: false }, breakpoints: [{ path: 'debug.py', lines: [2] }] })
  const paused = await until(async () => {
    const state = await ide({ op: 'execution.status', workspaceId })
    const current = state.debugSessions.find(entry => entry.id === debug.id)
    if (current?.phase === 'failed') throw new Error(`Python debugger: ${current.error}`)
    return current?.phase === 'paused' ? current : undefined
  }, 'Python breakpoint pause')
  const breakpoint = paused.breakpoints.find(value => value.verified && value.line === 2 && value.path === join(project, 'debug.py'))
  assert(breakpoint, JSON.stringify(paused.breakpoints))
  const threads = await ide({ op: 'debug.threads', workspaceId, debugId: debug.id })
  const threadId = paused.threadId ?? threads[0]?.id
  assert(Number.isInteger(threadId))
  const stack = await ide({ op: 'debug.stack', workspaceId, debugId: debug.id, threadId })
  const frame = stack.find(value => value.path === join(project, 'debug.py') && value.line === 2)
  assert(frame, JSON.stringify(stack))
  const watch = await ide({ op: 'debug.evaluate', workspaceId, debugId: debug.id, frameId: frame.id, expression: 'value', context: 'watch' })
  assert.equal(watch.result, '41')
  await ide({ op: 'debug.stop', workspaceId, debugId: debug.id })
  const terminated = await until(async () => {
    const state = await ide({ op: 'execution.status', workspaceId })
    return state.debugSessions.find(value => value.id === debug.id && value.phase === 'terminated')
  }, 'Python debugger stop')
  assert.equal(terminated.exit.stopped, true)
  checks.push({ name: 'Python verified breakpoint, stack, watch and Stop', passed: true, id: debug.id, breakpoint, frame,
    watch: watch.result, exit: terminated.exit })
  const result = await stopHost()
  assert.equal(result.code, 0, diagnostic)
  assert.equal(result.signal, null)
  orderlyShutdown = true
  checks.push({ name: 'Host stop plus stdin EOF closes with exit 0', passed: true, ...result })
} catch (error) {
  failure = error instanceof Error ? error.stack ?? error.message : String(error)
  process.exitCode = 1
} finally {
  try { await stopHost() }
  catch (error) {
    failure = `${failure ?? ''}\nCleanup: ${error instanceof Error ? error.message : String(error)}`.trim()
    process.exitCode = 1
  }
  lines?.close()
  let safeDiagnostic = diagnostic
  for (const secret of secrets) {
    if (!secret) continue
    safeDiagnostic = safeDiagnostic.replaceAll(secret, '[redacted]')
    if (failure) failure = failure.replaceAll(secret, '[redacted]')
  }
  console.log(JSON.stringify({ kind: 'installed-ide-no-network', passed: failure === undefined && orderlyShutdown,
    startedAt, completedAt: new Date().toISOString(), elapsedMs: performance.now() - started,
    hostPath, hostSha256, runtimePath: hostPath ? resolve(dirname(hostPath), '../..') : undefined,
    nodePath: process.execPath, nodeVersion: process.version, home, project, network, checks,
    orderlyShutdown, hostExit: childResult, ...(failure === undefined ? {} : { error: failure }),
    ...(safeDiagnostic ? { diagnostic: safeDiagnostic } : {}) }))
}
