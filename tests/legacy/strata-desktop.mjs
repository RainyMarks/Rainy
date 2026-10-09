/** Connect an explicitly supplied mock server through the packaged settings UI without loading model weights. */
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { createHash } from 'node:crypto'
import { createRequire } from 'node:module'

const repository = resolve(import.meta.dirname, '../../..')
const [dataArgument, executableArgument, mockArgument, kind = 'packaged', expectedText = 'RainyAgent connected'] = process.argv.slice(2)
assert(dataArgument && executableArgument && mockArgument,
  'Usage: node strata-desktop.mjs <isolated-data> <carrier-exe> <mock-base-url-or-port> [packaged|installed|relocated] [expected-mock-text]')
assert(['packaged', 'installed', 'relocated'].includes(kind), 'Evidence kind must describe the tested carrier location')
assert(expectedText.trim(), 'The mock response must contain an explicit expected text')
const mockURL = new URL(/^\d+$/u.test(mockArgument) ? `http://127.0.0.1:${mockArgument}/v1` : mockArgument)
assert.equal(mockURL.protocol, 'http:')
assert.equal(mockURL.hostname, '127.0.0.1')
assert(mockURL.port && Number(mockURL.port) >= 1024 && !mockURL.username && !mockURL.password && !mockURL.search && !mockURL.hash)
assert(/^\/v1\/?$/u.test(mockURL.pathname), 'The mock API URL must end in /v1')
mockURL.pathname = '/v1'
const data = resolve(dataArgument)
const executable = resolve(executableArgument)
const version = JSON.parse(await readFile(join(repository, 'apps/rainy-desktop/package.json'), 'utf8')).version
const evidence = join(repository, `validation/source-available-${version}`, `${kind}-strata-client`)
await mkdir(evidence, { recursive: true })
const readHealth = async () => {
  const response = await fetch(new URL('/health', mockURL), { signal: AbortSignal.timeout(3000), redirect: 'error' })
  assert.equal(response.status, 200)
  const health = await response.json()
  assert.equal(health.status, 'ok')
  assert.equal(health.service, 'strata')
  assert.equal(health.loaded, true)
  assert.equal(health.api_key, false)
  assert.equal(typeof health.model, 'string')
  assert(Number.isSafeInteger(health.max_context) && health.max_context >= 8192)
  return health
}
const health = await readHealth()
const [port, endpoint] = (await readFile(join(data, 'DevToolsActivePort'), 'utf8')).trim().split(/\r?\n/u)
const { chromium } = createRequire(join(repository, 'apps/web/package.json'))('playwright')
const browser = await chromium.connectOverCDP(`ws://127.0.0.1:${port}${endpoint}`)
const report = { kind: `${kind}-carrier-external-mock`, executable, data, version,
  mock: { baseURL: mockURL.href, model: health.model, contextWindow: health.max_context },
  checks: [], pageErrors: [], consoleErrors: [], screenshots: [],
  executableSha256: createHash('sha256').update(await readFile(executable)).digest('hex'),
  resourceManifestSha256: createHash('sha256').update(await readFile(join(dirname(executable), 'resources/release-manifest.signed.json'))).digest('hex') }
const until = async (read, description, milliseconds = 60000) => {
  const deadline = Date.now() + milliseconds
  while (Date.now() < deadline) {
    if (!browser.isConnected()) throw new Error(`Carrier disconnected: ${description}`)
    const value = await read()
    if (value) return value
    await new Promise(accept => setTimeout(accept, 100))
  }
  throw new Error(description)
}
let page
try {
  page = await until(() => browser.contexts().flatMap(context => context.pages())
    .find(candidate => candidate.url().startsWith('http://127.0.0.1:')), 'Native Host page did not load', 180000)
  page.setDefaultTimeout(30000)
  page.on('pageerror', error => report.pageErrors.push(error.message))
  page.on('console', message => { if (message.type() === 'error') report.consoleErrors.push(message.text().slice(0, 3000)) })
  await page.locator('[data-rainy-topbar]').waitFor()
  assert.equal(await page.evaluate(() => globalThis.__RAINY_AGENT__?.version), version)
  const target = await page.evaluate(() => globalThis.__RAINY_RUNTIME_NATIVE__.targets())
  assert.equal(target.current.kind, 'windows', 'The fixture connects the Windows loopback mock from a Windows Host')
  const before = await page.evaluate(() => globalThis.__RAINY_STRATA_NATIVE__.status())
  assert.equal(before.runtime.available, true)
  assert.equal(before.runtime.version, '0.1.39')
  assert.equal(before.settings.modelPath, '', 'Use a fresh isolated data directory without selected model weights')
  assert.equal(before.settings.mtpPath, '')
  assert.equal(before.model, null)
  assert.notEqual(before.server?.owned, true)
  const controlURL = new URL('/rainy/control', page.url()).href
  const readControl = async () => {
    const response = await page.context().request.get(controlURL)
    assert.equal(response.status(), 200)
    return response.json()
  }
  const initial = await readControl()
  assert(!initial.models.some(model => model.provider === 'rainy-strata'), 'Use an isolated profile without a prior Strata provider')
  report.checks.push('the packaged native bridge exposes bundled Strata 0.1.39 with no selected weights or owned engine')
  await page.locator('[data-rainy-topbar]').getByRole('button', { name: '设置', exact: true }).click()
  const settings = page.getByRole('dialog', { name: '设置', exact: true })
  await settings.getByRole('button', { name: '模型', exact: true }).click()
  const models = settings.locator('[data-rainy-settings="models"]')
  const card = models.locator('[data-rainy-strata]')
  await card.locator('summary').first().click()
  await card.getByText('内置 Strata 与 Python 已就绪', { exact: false }).waitFor()
  await card.getByRole('spinbutton', { name: '本地端口', exact: true }).fill(mockURL.port)
  await card.getByRole('button', { name: '保存 Strata 配置', exact: true }).click()
  const external = await until(async () => {
    const state = await page.evaluate(() => globalThis.__RAINY_STRATA_NATIVE__.status())
    return state.settings.port === Number(mockURL.port) && state.phase === 'external' && state.server?.loaded ? state : undefined
  }, 'The native bridge did not discover the explicitly supplied mock server')
  assert.equal(external.server.owned, false)
  assert.equal(external.server.model, health.model)
  assert.equal(external.server.contextWindow, health.max_context)
  assert.equal(external.settings.modelPath, '')
  assert.equal(external.model, null)
  assert.equal(await card.getByRole('button', { name: '启动本地模型', exact: true }).isDisabled(), true)
  assert.equal(await card.getByRole('button', { name: '停止本地模型', exact: true }).isDisabled(), true)
  await until(async () => await card.getByRole('button', { name: '刷新', exact: true }).isEnabled(), 'Strata settings did not finish saving')
  await card.getByRole('button', { name: '刷新', exact: true }).click()
  const connect = card.getByRole('button', { name: '连接并设为默认', exact: true })
  await until(() => connect.isEnabled(), 'The actual settings card did not allow connecting the loaded external mock')
  await page.screenshot({ path: join(evidence, '01-external-strata.png') }); report.screenshots.push('01-external-strata.png')
  await connect.click()
  const saved = await until(async () => {
    const status = await readControl()
    const model = status.models.find(model => model.provider === 'rainy-strata' && model.model === health.model)
    return model && status.selected?.provider === 'rainy-strata' && status.selected.model === health.model ? model : undefined
  }, 'The native connection did not configure and select the actual Strata model in the Host')
  assert.equal(saved.baseURL, mockURL.href)
  assert.equal(saved.contextWindow, health.max_context)
  assert.equal(saved.api, 'openai-completions')
  assert.equal(saved.local, true)
  await until(async () => (await models.getByRole('textbox', { name: '供应商 ID', exact: true }).inputValue()) === 'rainy-strata',
    'The settings form did not refresh after the native Host connection')
  assert.equal(await models.getByRole('textbox', { name: '模型 ID', exact: true }).inputValue(), health.model)
  assert.equal(await models.getByRole('spinbutton', { name: '实际上下文长度', exact: true }).inputValue(), String(health.max_context))
  assert.equal((await models.getByRole('button', { name: '已保存的模型配置', exact: true }).innerText()).trim(), `rainy-strata/${health.model}`)
  report.savedModel = { provider: saved.provider, model: saved.model, baseURL: saved.baseURL,
    contextWindow: saved.contextWindow, maxTokens: saved.maxTokens, api: saved.api, thinking: saved.thinking }
  report.checks.push('clicking the real Strata Connect button authenticates through the native bridge, saves the local provider, and refreshes the selected model UI')
  await models.getByRole('textbox', { name: '供应商 ID', exact: true }).scrollIntoViewIfNeeded()
  await page.screenshot({ path: join(evidence, '02-selected-strata.png') }); report.screenshots.push('02-selected-strata.png')
  const responsePromise = page.waitForResponse(response => {
    if (response.url() !== controlURL || response.request().method() !== 'POST') return false
    try { return response.request().postDataJSON()?.method === 'probe-model' } catch { return false }
  }, { timeout: 210000 })
  await models.getByRole('button', { name: '验证流式与工具调用', exact: true }).click()
  const response = await responsePromise
  assert.equal(response.status(), 200)
  const probe = await response.json()
  assert.equal(probe.result?.stream, true, JSON.stringify(probe))
  assert(probe.result.text.includes(expectedText), 'The real Host adapter did not receive the expected mock completion')
  report.probe = { stream: probe.result.stream, toolCall: probe.result.toolCall, text: probe.result.text }
  await models.getByText(/流式输出：通过/u).waitFor()
  await models.getByText(/流式输出：通过/u).scrollIntoViewIfNeeded()
  await page.screenshot({ path: join(evidence, '03-mock-stream-probe.png') }); report.screenshots.push('03-mock-stream-probe.png')
  report.checks.push('the visible model probe streams the expected mock text through the configured Host adapter; tool calls are recorded separately')
  const after = await page.evaluate(() => globalThis.__RAINY_STRATA_NATIVE__.status())
  assert.equal(after.phase, 'external')
  assert.equal(after.server.owned, false)
  assert.equal(after.settings.modelPath, '')
  assert.equal((await readHealth()).model, health.model)
  assert.deepEqual(report.pageErrors, [])
  assert(!report.consoleErrors.some(message => message.includes('slot entry crashed')), 'A settings view failed inside its error boundary')
  report.checks.push('the external mock remains loaded and no native model process is claimed or stopped')
  report.passed = true
} catch (error) {
  report.passed = false
  report.error = String(error)
  if (page && !page.isClosed()) await page.screenshot({ path: join(evidence, 'failure.png') }).catch(() => {})
  throw error
} finally {
  await writeFile(join(evidence, 'result.json'), JSON.stringify(report, null, 2) + '\n')
  if (page && !page.isClosed()) await page.evaluate(() => { window.close() }).catch(() => {})
  await browser.close()
}
console.log(JSON.stringify({ passed: report.passed, checks: report.checks, evidence }))
