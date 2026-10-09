/** Exercise the shipped tool modules, local assets, persistence and feedback with benign fixture text. */
import assert from 'node:assert/strict'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const runtime = process.argv[2]
if (!runtime) throw new Error('Pass a private staged Linux runtime directory')
const output = resolve('apps/rainy-desktop/validation/icesky-browser')
await mkdir(output, { recursive: true })
const harness = await openWorkbenchHarness({ runtime, fixture: process.argv.includes('--fixture') })
const report = { runtime, fixture: harness.ready.fixtureSessionIds ?? [], tools: [], errors: [], consoleErrors: [], blockedExternalHosts: [] }
const page = await harness.context.newPage()
page.on('pageerror', error => report.errors.push(error.message))
page.on('console', message => { if (message.type() === 'error') report.consoleErrors.push(message.text()) })
try {
  await page.goto(`${harness.origin}/rainy/icesky/index.html?embed=rainy`)
  await page.waitForFunction(() => typeof window.app?.openTool === 'function' && window.app.toolLoading === false)
  assert.equal(await page.evaluate(() => window.toolRegistry.implementations.size), 1)
  report.initialTool = await page.evaluate(() => window.app.activeTab)
  const ids = await page.evaluate(() => window.app.registeredTools.filter(tool => !tool.hidden).map(tool => tool.id))
  assert.equal(ids.length, 22)
  for (const id of ids) {
    console.log(`tool ${id}`)
    await page.evaluate(async id => { await window.app.openTool(id) }, id)
    await page.waitForFunction(id => window.app.activeTab === id && window.app.getToolView(id)?.$el?.isConnected
      && window.app.getToolView(id).$el.querySelectorAll('input,textarea,button,select').length > 0, id)
    report.tools.push(await page.evaluate(id => {
      const view = window.app.getToolView(id)
      return { id, rendered: view.$el.querySelectorAll('input,textarea,button,select').length,
        error: window.app.toolError, missingFields: Object.keys(view.$data).filter(key => /Error$/.test(key) && view[key]).map(key => [key, view[key]]) }
    }, id))
    assert.equal(report.tools.at(-1).error, '', id)
  }
  await page.evaluate(async () => { await window.app.openTool('decoder') })
  console.log('decoder input')
  await page.locator('#decoder-input').fill('SGVsbG8sIOS4lueVjCE=')
  await page.getByRole('button', { name: '计算', exact: true }).click()
  await page.waitForFunction(() => !window.app.getToolView('decoder').decoderComputing && !!window.app.getToolView('decoder').decoderOutput)
  assert.equal(await page.locator('#decoder-output').inputValue(), 'Hello, 世界!')
  console.log('decoder passed')
  await page.evaluate(async () => { await window.app.openTool('tokenizer') })
  const longText = 'Ordinary Unicode 示例🙂 '.repeat(6000)
  await page.locator('#tokenizer-input').fill(longText)
  await page.waitForFunction(() => window.app.getToolView('tokenizer').tokenizerManualRequired)
  await page.locator('#tokenizer-input').press('Control+Enter')
  await page.waitForFunction(() => window.app.getToolView('tokenizer').tokenizerTotalCount > 100 && !window.app.getToolView('tokenizer').tokenizerComputing)
  const first = await page.evaluate(() => {
    const view = window.app.getToolView('tokenizer')
    return { count: view.tokenizerTotalCount, groups: view.tokenizerGroups.length, tokens: view.tokenizerTokens.length, page: view.tokenizerPage }
  })
  assert(first.groups <= 100 && first.tokens <= 100)
  report.keyboardManualCompute = true
  await page.keyboard.press('Control+k')
  await page.locator('#command-palette-input').waitFor({ state: 'visible' })
  await page.keyboard.press('Escape')
  await page.waitForFunction(() => !window.app.showCommandPalette)
  report.keyboardSearch = true
  console.log('tokenizer passed')
  await page.getByRole('button', { name: '下一页', exact: true }).click()
  await page.waitForFunction(() => window.app.getToolView('tokenizer').tokenizerPage === 1)
  report.tokenizer = { first, next: await page.evaluate(() => {
    const view = window.app.getToolView('tokenizer')
    return { count: view.tokenizerTotalCount, start: view.tokenizerGroups[0]?.start ?? view.tokenizerTokens[0]?.index }
  }) }
  await page.screenshot({ path: resolve(output, 'tokenizer-light.png'), fullPage: false })

  const beforeHistory = await page.evaluate(() => window.app.copyHistory.length)
  console.log('clipboard')
  await page.evaluate(() => { Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async () => { throw new DOMException('Fixture denied', 'NotAllowedError') } } }) })
  assert.equal(await page.evaluate(() => window.app.copyToClipboard('clipboard failure fixture')), false)
  assert.equal(await page.evaluate(() => window.app.copyHistory.length), beforeHistory)
  await page.evaluate(() => { Object.defineProperty(navigator, 'clipboard', { configurable: true, value: { writeText: async () => {} } }) })
  assert.equal(await page.evaluate(() => window.app.getToolView('tokenizer').copyToClipboard('clipboard success fixture')), true)
  const lastHistory = await page.evaluate(() => window.app.copyHistory[0])
  assert.equal(lastHistory.sourceId, 'tokenizer')
  assert(!lastHistory.source.includes('Unknown'))
  const afterCopy = await page.evaluate(() => window.app.copyHistory.length)
  assert.equal(await page.evaluate(() => window.app.copyToClipboard(window.app.copyHistory[0].content, true)), true)
  assert.equal(await page.evaluate(() => window.app.copyHistory.length), afterCopy)
  report.clipboard = { deniedDidNotAddHistory: true, sourceId: lastHistory.sourceId }

  await page.evaluate(async () => {
    await window.app.openTool('transforms', { transformInput: 'Saved benign draft 示例🙂' })
    await window.IceSkyRuntime.flush()
  })
  console.log('reload')
  await page.reload()
  await page.waitForFunction(() => window.app && !window.app.toolLoading && !!window.app.getToolView('transforms'))
  assert.equal(await page.locator('#transform-input').inputValue(), 'Saved benign draft 示例🙂')
  report.reloadRestore = true

  console.log('save failure and recovery')
  await page.evaluate(async () => { await window.IceSkyRuntime.flush() })
  const savedBeforeFailure = await harness.context.request.get(`${harness.origin}/rainy/icesky/state?scope=standalone`).then(value => value.json())
  const failSave = route => route.request().method() === 'PUT'
    ? route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Fixture storage unavailable' }) })
    : route.continue()
  await page.route('**/rainy/icesky/state?scope=standalone', failSave)
  await page.locator('#transform-input').fill('Unsaved recovery fixture 示例🙂')
  assert.equal(await page.evaluate(async () => { try { await window.IceSkyRuntime.flush(); return true } catch { return false } }), false)
  assert.equal(await page.evaluate(() => window.app.saveState), 'error')
  const diskDuringFailure = await harness.context.request.get(`${harness.origin}/rainy/icesky/state?scope=standalone`).then(value => value.json())
  assert.equal(diskDuringFailure.data.tools.transforms.fields.transformInput, savedBeforeFailure.data.tools.transforms.fields.transformInput)
  const downloading = page.waitForEvent('download')
  await page.getByRole('button', { name: '导出草稿', exact: true }).click()
  const download = await downloading
  const exported = JSON.parse(await readFile(await download.path(), 'utf8'))
  assert.equal(exported.tools.transforms.fields.transformInput, 'Unsaved recovery fixture 示例🙂')
  await page.unroute('**/rainy/icesky/state?scope=standalone', failSave)
  await page.getByRole('button', { name: '重试', exact: true }).click()
  await page.waitForFunction(() => window.app.saveState === 'saved')
  report.saveRecovery = { diskPreserved: true, memoryExported: true, retrySaved: true }

  console.log('lazy resource retry')
  const failTemplate = route => route.fulfill({ status: 503, body: 'Fixture unavailable' })
  await page.route('**/templates/tokenizer.html', failTemplate)
  const failure = await page.evaluate(async () => { try { await window.app.openTool('tokenizer'); return '' } catch (error) { return error.message } })
  assert(failure)
  assert(await page.evaluate(() => window.app.toolError))
  await page.unroute('**/templates/tokenizer.html', failTemplate)
  await page.getByRole('button', { name: '重试', exact: true }).click()
  await page.waitForFunction(() => !window.app.toolLoading && !window.app.toolError && !!window.app.getToolView('tokenizer'))
  await page.locator('#tokenizer-input').fill('Line one\n中文🙂\nLine three')
  await page.waitForFunction(() => window.app.getToolView('tokenizer').tokenizerTotalCount > 0 && !window.app.getToolView('tokenizer').tokenizerComputing)
  report.lazyRetryAndMultiline = true
  report.blockedExternalHosts = [...new Set(harness.blocked)]
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify(report))
} catch (error) {
  report.failure = error.stack
  report.blockedExternalHosts = [...new Set(harness.blocked)]
  report.pageText = (await page.locator('body').innerText().catch(() => '')).slice(0,1000)
  await page.screenshot({ path: resolve(output, 'failure.png'), fullPage: false }).catch(() => {})
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify({ failure: error.message, errors: report.errors, consoleErrors: report.consoleErrors.slice(-10), output }))
  process.exitCode = 1
} finally { await harness.stop() }
