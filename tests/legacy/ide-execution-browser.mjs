/** Real frontend execution acceptance in an isolated Rainy profile. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { randomUUID, createHash } from 'node:crypto'
import { spawnSync } from 'node:child_process'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro, user, scope = 'all'] = process.argv.slice(2)
assert(runtime && distro && user, 'Pass the prepared runtime, isolated distribution and test user')
assert(scope === 'all' || scope === 'venv', 'Scope must be all or venv')
const output = resolve(`validation/ide-runtime/execution-browser${scope === 'venv' ? '-venv' : ''}`)
await mkdir(output, { recursive: true })
const home = `/var/tmp/rainy-ide-execution-browser-${randomUUID()}`
const startedAt = new Date().toISOString()
const harness = await openWorkbenchHarness({ runtime, distro, user, home, fixture: true, viewport: { width: 1680, height: 1080 } })
const browserVersion = harness.browser.version()
let stopped = false
const page = await harness.context.newPage()
page.setDefaultTimeout(30000)
const checks = []
const errors = []
const requests = []
const diagnostics = []
const languageConfigurations = []
const replies = []
const assets = new Map()
page.on('pageerror', error => errors.push(error.stack ?? error.message))
page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
page.on('response', response => {
  if (response.request().method() === 'GET' && response.headers()['content-type']?.includes('javascript')) {
    const pending = response.body().then(body => {
      assets.set(new URL(response.url()).pathname, createHash('sha256').update(body).digest('hex'))
    }).catch(error => { errors.push(`Asset capture: ${error.message}`) })
    replies.push(pending)
  }
  if (!response.url().endsWith('/rainy/ide') || response.request().method() !== 'POST') return
  const request = response.request().postDataJSON()
  if (request.op === 'execution.poll') return
  const pending = response.json().then(result => {
    requests.push({ op: request.op, ...(request.action ? { action: request.action } : {}),
      ...(request.expression ? { expression: request.expression, context: request.context } : {}),
      status: response.status(), ok: result.ok, ...(result.ok ? {} : { error: result.error }) })
  }).catch(error => { errors.push(`Response capture: ${error.message}`) })
  replies.push(pending)
})
page.on('websocket', socket => {
  const diagnosticRequests = new Map()
  socket.on('framereceived', frame => {
    try {
      const value = JSON.parse(String(frame.payload))
      if (value.method === 'textDocument/publishDiagnostics') diagnostics.push({ receivedAt: Date.now(), ...value.params })
      const uri = diagnosticRequests.get(value.id)
      if (uri !== undefined) {
        diagnosticRequests.delete(value.id)
        if (value.result?.kind === 'full') diagnostics.push({ receivedAt: Date.now(), uri, diagnostics: value.result.items })
      }
    } catch (error) { if (!(error instanceof SyntaxError)) throw error }
  })
  socket.on('framesent', frame => {
    try {
      const value = JSON.parse(String(frame.payload))
      if (value.method === 'textDocument/diagnostic') diagnosticRequests.set(value.id, value.params.textDocument.uri)
      if (value.method === 'initialize' || value.method === 'workspace/didChangeConfiguration') {
        languageConfigurations.push({ method: value.method, ...(value.method === 'initialize'
          ? { rootUri: value.params.rootUri, initializationOptions: value.params.initializationOptions }
          : { settings: value.params.settings }) })
      }
    } catch (error) { if (!(error instanceof SyntaxError)) throw error }
  })
})
function record(check) { checks.push(check); console.log(JSON.stringify({ passed: true, check: check.name })) }
let workspaceId
let workspacePath
async function api(body) {
  const response = await harness.context.request.post(`${harness.origin}/rainy/ide`, { data: body })
  const result = await response.json()
  assert.equal(response.status(), 200, JSON.stringify(result))
  assert.equal(result.ok, true, JSON.stringify(result))
  return result.value
}
async function until(read, description, timeout = 30000) {
  const deadline = Date.now() + timeout
  while (Date.now() < deadline) {
    const value = await read()
    if (value) return value
    await new Promise(accept => setTimeout(accept, 100))
  }
  throw new Error(description)
}
async function responseFor(op, action) {
  const response = await page.waitForResponse(response => response.url().endsWith('/rainy/ide')
    && response.request().method() === 'POST' && response.request().postDataJSON()?.op === op
    && (action === undefined || response.request().postDataJSON()?.action === action))
  const result = await response.json()
  assert.equal(result.ok, true, JSON.stringify(result))
  return result.value
}
async function openFile(name) {
  await page.getByRole('treeitem').filter({ hasText: name }).click()
  await page.locator('[data-rainy-monaco] .monaco-editor:visible').first().waitFor({ timeout: 60000 })
  await until(async () => (await api({ op: 'state.read', workspaceId })).data.activePath === name, `Active file did not become ${name}`)
}
async function editFile(content) {
  await page.locator('[data-rainy-monaco] .view-lines:visible').first().click({ position: { x: 70, y: 12 } })
  await page.keyboard.press('Control+a')
  await page.keyboard.insertText(content)
  const saved = responseFor('files.save')
  await page.keyboard.press('Control+s')
  await saved
}
async function status() { return api({ op: 'execution.status', workspaceId }) }
async function poll() { return api({ op: 'execution.poll', workspaceId, cursor: 0 }) }
function textFor(events, id) { return events.filter(event => event.kind === 'output' && event.operationId === id).map(event => event.text).join('') }
async function screenshot(name) { await page.screenshot({ path: resolve(output, `${name}.png`) }) }
async function configure({ name, program, executable = '' }) {
  await page.locator('[data-rainy-topbar]').getByRole('button', { name: /^(文件|File)$/ }).click()
  await page.getByRole('menuitem', { name: /^(运行配置|Run configurations)$/ }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel(/^(配置名称|Configuration name)$/).fill(name)
  await dialog.getByLabel(/^(程序路径|Program path)$/).fill(program)
  await dialog.getByLabel(/^(解释器或编译器|Interpreter or compiler)/).fill(executable)
  const saved = responseFor('state.save')
  await dialog.getByRole('button', { name: /^(保存|Save)$/ }).click()
  await saved
  await dialog.waitFor({ state: 'hidden' })
}
try {
  const workspace = (await api({ op: 'workspaces.list' }))[0]
  assert(workspace)
  workspaceId = workspace.workspaceId
  workspacePath = workspace.path
  await api({ op: 'files.create', workspaceId, path: 'main.py', content: 'value = 41\nprint(value + 1)\n' })
  await page.goto(harness.origin, { waitUntil: 'load' })
  const welcome = page.locator('[class*="onboardingOverlay"]')
  if (await welcome.count()) await welcome.getByRole('button').click()
  await page.locator('[data-rainy-ide]').waitFor({ timeout: 60000 })
  await page.getByRole('combobox', { name: /^(工作区|Workspace)$/ }).selectOption(workspaceId)
  await openFile('main.py')
  if (scope === 'all') {
    const started = responseFor('run.start')
    await page.locator('[data-rainy-topbar]').getByRole('button', { name: /^(运行|Run)$/ }).click()
    const normal = await started
    const normalExit = await until(async () => (await status()).runs.find(run => run.id === normal.id && run.phase === 'exited'), 'Normal UI run did not exit')
    assert.equal(normalExit.exit.exitCode, 0)
    await page.getByRole('tab', { name: /^(输出|Output)$/ }).click()
    await page.locator('pre:visible').filter({ hasText: '42' }).waitFor()
    const normalOutput = textFor((await poll()).events, normal.id)
    assert.match(normalOutput, /(?:^|\r?\n)42\r?\n/)
    record({ name: 'top Run executes Python and renders 42', runId: normal.id, exit: normalExit.exit, output: normalOutput })
    await screenshot('run-output')

    await editFile('value = 41\nvalue += 1\nprint(value, flush=True)\nimport time\ntime.sleep(60)\n')
    const line = page.locator('[data-rainy-monaco] .line-numbers:visible').filter({ hasText: /^2$/ }).first()
    const lineBox = await line.boundingBox()
    const marginBox = await page.locator('[data-rainy-monaco] .glyph-margin:visible').first().boundingBox()
    assert(lineBox && marginBox)
    await page.mouse.click(marginBox.x + marginBox.width / 2, lineBox.y + lineBox.height / 2)
    await page.locator('.rainy-monaco-breakpoint:visible').waitFor()
    await until(async () => (await api({ op: 'state.read', workspaceId })).data.execution?.breakpoints.some(source => source.path === 'main.py' && source.lines.includes(2)), 'Gutter breakpoint was not retained')
    const debugging = responseFor('debug.start')
    await page.locator('[data-rainy-topbar]').getByRole('button', { name: /^(调试|Debug)$/ }).click()
    const debug = await debugging
    const paused = await until(async () => (await status()).debugSessions.find(session => session.id === debug.id && session.phase === 'paused'), 'UI debugger did not pause at the gutter breakpoint', 60000)
    assert(paused.breakpoints.some(point => point.verified && point.line === 2))
    await page.getByRole('button', { name: /main\.py:2/ }).first().waitFor()
    const variableColumn = page.locator('section').filter({ has: page.getByRole('heading', { name: /^(变量|Variables)$/ }) })
    const watchColumn = page.locator('section').filter({ has: page.getByRole('heading', { name: /^(监视|Watches)$/ }) })
    await variableColumn.getByRole('button', { name: 'value: 41', exact: true }).first().waitFor()
    const evaluated = responseFor('debug.evaluate')
    await page.getByRole('textbox', { name: /^(添加监视表达式|Add watch expression)$/ }).fill('value')
    await page.getByRole('textbox', { name: /^(添加监视表达式|Add watch expression)$/ }).press('Enter')
    assert.equal((await evaluated).result, '41')
    await watchColumn.getByRole('button', { name: 'value: 41', exact: true }).waitFor()
    await screenshot('debug-breakpoint-watch')
    const stepped = responseFor('debug.control', 'next')
    await page.getByRole('button', { name: /^(单步跳过|Step over)$/ }).click()
    await stepped
    await page.getByRole('button', { name: /main\.py:3/ }).first().waitFor()
    await variableColumn.getByRole('button', { name: 'value: 42', exact: true }).first().waitFor()
    await watchColumn.getByRole('button', { name: 'value: 42', exact: true }).waitFor()
    const continued = responseFor('debug.control', 'continue')
    await page.getByRole('button', { name: /^(继续|Continue)$/ }).click()
    await continued
    await until(async () => textFor((await poll()).events, debug.id).includes('42'), 'Continued debuggee did not produce 42')
    const stoppedDebug = responseFor('debug.stop')
    await page.getByRole('button', { name: /^(停止|Stop)$/ }).click()
    await stoppedDebug
    const debugExit = await until(async () => (await status()).debugSessions.find(session => session.id === debug.id && session.phase === 'terminated'), 'UI stop did not terminate the debuggee')
    assert.equal(debugExit.exit.stopped, true)
    record({ name: 'gutter breakpoint, top Debug, scopes, watch, step over, continue and stop', debugId: debug.id, breakpoint: paused.breakpoints, watchBefore: '41', watchAfter: '42', exit: debugExit.exit })

    const longStart = responseFor('run.start')
    await page.locator('[data-rainy-topbar]').getByRole('button', { name: /^(运行|Run)$/ }).click()
    const long = await longStart
    await until(async () => (await status()).runs.some(run => run.id === long.id && run.phase === 'running'), 'Long UI run did not enter running')
    const runStopped = responseFor('run.stop')
    await page.getByRole('button', { name: /^(停止|Stop)$/ }).click()
    await runStopped
    const longExit = await until(async () => (await status()).runs.find(run => run.id === long.id && run.phase === 'exited'), 'Long UI run did not stop')
    assert.equal(longExit.exit.stopped, true)
    record({ name: 'Run Stop terminates a sleeping Python program', runId: long.id, exit: longExit.exit })

    const terminalStart = responseFor('terminal.start')
    await page.getByRole('button', { name: /^(新建终端|New terminal)$/ }).click()
    const terminal = await terminalStart
    const terminalInput = page.locator('[data-rainy-terminal] textarea.xterm-helper-textarea')
    await terminalInput.focus()
    await page.keyboard.type("printf 'RAINY_TERMINAL_%s\\n' 42", { delay: 25 })
    await page.keyboard.press('Enter')
    await until(async () => textFor((await poll()).events, terminal.id).includes('RAINY_TERMINAL_42'), 'Typed terminal command did not produce its marker')
    await screenshot('terminal-input')
    const terminalStopped = responseFor('terminal.stop')
    await page.getByRole('button', { name: /^(停止|Stop)$/ }).click()
    await terminalStopped
    record({ name: 'xterm keyboard input reaches the managed terminal', terminalId: terminal.id, marker: 'RAINY_TERMINAL_42' })
  }

  const setup = spawnSync('wsl.exe', ['-d', distro, '-u', user, '--exec', 'python3', '-c',
    'import pathlib,sys,venv; root=pathlib.Path(sys.argv[1]); env=root/"chosen-venv"; venv.EnvBuilder(with_pip=False).create(env); site=next((env/"lib").glob("python*/site-packages")); (site/"rainy_ide_browser_module.py").write_text("def answer() -> int:\\n    return 42\\n"); print(env/"bin/python")', workspacePath],
  { windowsHide: true, encoding: 'utf8' })
  assert.equal(setup.status, 0, setup.stderr)
  const interpreter = setup.stdout.trim()
  const venvSource = 'import sys\nfrom rainy_ide_browser_module import answer\nresult: str = answer()\nprint("VENV=" + sys.prefix)\nprint(answer())\n'
  await api({ op: 'files.create', workspaceId, path: 'venv.py', content: venvSource })
  await page.getByTitle(/^(刷新|Refresh)$/).click()
  await openFile('venv.py')
  const baseline = await until(async () => diagnostics.findLast(entry => entry.uri.endsWith('/venv.py') && entry.diagnostics.some(value => value.message.includes('rainy_ide_browser_module'))), 'Pyright baseline did not report the module outside its default environment', 60000)
  await configure({ name: 'Chosen project environment', program: 'venv.py', executable: interpreter })
  const typed = await until(async () => {
    const latest = diagnostics.findLast(entry => entry.uri.endsWith('/venv.py'))
    return latest && !latest.diagnostics.some(value => value.message.includes('could not be resolved'))
      && latest.diagnostics.some(value => value.message.includes('int') && value.message.includes('str')) && latest
  }, 'Pyright did not resolve the selected environment and retain the real type mismatch', 60000)
  if (await page.getByRole('tab', { name: /^(问题|Problems)/ }).count() === 0) {
    await page.locator('[data-rainy-topbar]').getByRole('button', { name: /^(视图|View)$/ }).click()
    await page.getByRole('menuitem', { name: /^(显示或隐藏底部面板|Show or hide bottom panel)$/ }).click()
  }
  await page.getByRole('tab', { name: /^(问题|Problems)/ }).click()
  await page.getByRole('button').filter({ hasText: /Type "int" is not assignable/ }).first().waitFor()
  await screenshot('venv-pyright')
  const venvStart = responseFor('run.start')
  await page.locator('[data-rainy-topbar]').getByRole('button', { name: /^(运行|Run)$/ }).click()
  const venvRun = await venvStart
  assert.equal(venvRun.spec.launch.argv[0], interpreter)
  const venvExit = await until(async () => (await status()).runs.find(run => run.id === venvRun.id && run.phase === 'exited'), 'Selected environment run did not exit')
  assert.equal(venvExit.exit.exitCode, 0)
  const venvOutput = textFor((await poll()).events, venvRun.id)
  assert(venvOutput.includes(`VENV=${workspacePath}/chosen-venv`), venvOutput)
  assert.match(venvOutput, /42\r?\n/)
  await page.getByRole('tab', { name: /^(输出|Output)$/ }).click()
  await page.locator('pre:visible').filter({ hasText: 'VENV=' }).waitFor()
  await screenshot('venv-run')
  record({ name: 'run configuration selects the project venv for execution and Pyright', interpreter, runId: venvRun.id,
    baselineDiagnostics: baseline.diagnostics, selectedDiagnostics: typed.diagnostics, output: venvOutput, exit: venvExit.exit })
  await Promise.all(replies)
  assert.deepEqual(harness.blocked, [])
  assert.deepEqual(errors, [])
  assert(requests.every(request => request.ok && request.status === 200), JSON.stringify(requests.filter(request => !request.ok)))
  await harness.stop()
  stopped = true
  await writeFile(resolve(output, 'acceptance.json'), JSON.stringify({ passed: true, scope, runtime, distro, user, home, workspaceId, workspacePath,
    startedAt, completedAt: new Date().toISOString(), browser: 'headless Microsoft Edge', browserVersion,
    profileShutdownAwaited: true, checks, requests, errors, externalRequestsBlocked: harness.blocked,
    sourceFixtureSha256: createHash('sha256').update(venvSource).digest('hex'), languageConfigurations, assets: Object.fromEntries(assets) }, null, 2) + '\n')
  console.log(JSON.stringify({ passed: true, checks, report: resolve(output, 'acceptance.json') }))
} catch (error) {
  await screenshot('failure').catch(() => {})
  await writeFile(resolve(output, 'failure.txt'), `${String(error)}\n${errors.join('\n')}\n${await page.locator('body').innerText().catch(() => '')}`)
  await writeFile(resolve(output, 'acceptance.json'), JSON.stringify({ passed: false, scope, runtime, distro, user, home, workspaceId, workspacePath,
    checks, requests, errors, error: String(error), diagnostics, languageConfigurations, assets: Object.fromEntries(assets) }, null, 2) + '\n')
  throw error
} finally { if (!stopped) await harness.stop() }
