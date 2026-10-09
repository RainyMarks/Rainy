/** Main-window draft isolation and same-home recovery with private benign chat fixtures. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const runtime = process.argv[2]
if (!runtime) throw new Error('Pass the private staged Linux runtime directory.')
const output = resolve('apps/rainy-desktop/validation/icesky-contexts')
await mkdir(output, { recursive: true })
const report = { runtime, checks: [], errors: [] }
let harness
let page
async function record(name, extra = {}) {
  report.checks.push({ name, ...extra })
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify({ passed: name, ...extra }))
}
async function start(home, fixture = true) {
  harness = await openWorkbenchHarness({ runtime, home, fixture })
  page = await harness.context.newPage()
  page.on('pageerror', error => report.errors.push(error.message))
  await page.goto(harness.origin)
  await page.locator('[data-rainy-rail]').waitFor({ state: 'visible' })
  if (fixture) {
    const extra = page.getByRole('button', { name: '展开其余 5 个会话', exact: true })
    await extra.waitFor({ state: 'visible' })
    await extra.click()
  }
}
async function workbench(context) {
  await page.getByRole('button', { name: 'CTF 工具', exact: true }).click()
  const carrier = page.locator('[data-rainy-ctf-workbench] iframe')
  await carrier.waitFor({ state: 'visible' })
  const handle = await carrier.elementHandle()
  const frame = await handle.contentFrame()
  if (!frame) throw new Error('The real workbench iframe did not attach.')
  try {
    await frame.waitForFunction(context => window.app?.toolLoading === false
      && window.IceSkyRuntime?.context?.kind === context.kind
      && (context.kind === 'standalone' || window.IceSkyRuntime.context.id === context.id), context)
  } catch (error) {
    const details = await frame.evaluate(() => ({ context: window.IceSkyRuntime?.context,
      toolLoading: window.app?.toolLoading, activeTab: window.app?.activeTab, body: document.body.innerText.slice(0, 700) }))
    throw new Error(`Workbench context did not become ready: ${JSON.stringify(details)}`, { cause: error })
  }
  if (await frame.evaluate(() => window.app.activeTab !== 'transforms')) {
    await frame.getByRole('button', { name: '全部工具', exact: true }).click()
    await frame.getByRole('tab', { name: /文本变换/ }).first().click()
  }
  await frame.locator('#transform-input').waitFor({ state: 'visible' })
  return frame
}
async function chat(id) {
  const row = page.locator(`[data-row-key="session:${id}"]`)
  if (await row.count() === 0) await page.getByRole('button', { name: '展开其余 5 个会话', exact: true }).click()
  await row.click()
  return workbench({ kind: 'session', id })
}
async function save(frame, text) {
  await frame.locator('#transform-input').fill(text)
  await frame.evaluate(() => window.IceSkyRuntime.flush())
  assert.equal(await frame.locator('#transform-input').inputValue(), text)
}
async function richText(frame) {
  await frame.getByRole('button', { name: '全部工具', exact: true }).click()
  await frame.getByRole('tab', { name: /富文本/ }).first().click()
  await frame.locator('input[type="file"]').first().waitFor({ state: 'attached' })
}
try {
  await start()
  report.home = harness.home
  report.initialOrigin = harness.origin
  report.fixtureSeedRequests = harness.ready.modelRequests
  const ids = harness.ready.fixtureSessionIds
  assert.equal(ids.length, 10)
  const standalone = await workbench({ kind: 'standalone' })
  await save(standalone, 'Standalone benign draft 示例🙂')
  await record('standalone draft saved')
  const first = await chat(ids[0])
  assert.notEqual(await first.locator('#transform-input').inputValue(), 'Standalone benign draft 示例🙂')
  await save(first, 'Chat one benign draft 一🙂')
  await record('chat 1 draft saved')
  const second = await chat(ids[1])
  assert.notEqual(await second.locator('#transform-input').inputValue(), 'Chat one benign draft 一🙂')
  await save(second, 'Chat two benign draft 二🙂')
  await record('chat 2 draft isolated and saved')
  const restored = await chat(ids[0])
  assert.equal(await restored.locator('#transform-input').inputValue(), 'Chat one benign draft 一🙂')
  assert.equal(await page.locator('[data-rainy-ctf-workbench] iframe').count(), 1)
  await restored.evaluate(() => window.IceSkyRuntime.flush())
  await page.screenshot({ path: resolve(output, 'chat-one-restored.png') })
  await record('chat 1 draft restored after 1→2→1', { iframeCount: 1 })
  const home = harness.home
  const origin = harness.origin
  await harness.stop()
  harness = undefined
  await start(home)
  assert.notEqual(harness.origin, origin)
  report.restartOrigin = harness.origin
  report.restartReusedSessions = harness.ready.reusedSessions
  report.restartSeedRequests = harness.ready.modelRequests
  const restarted = await chat(ids[0])
  assert.equal(await restarted.locator('#transform-input').inputValue(), 'Chat one benign draft 一🙂')
  assert.equal(report.restartReusedSessions, 10)
  assert.equal(report.restartSeedRequests, 0)
  await page.screenshot({ path: resolve(output, 'chat-one-new-port.png') })
  await record('same home/new port restored chat 1 draft', { reusedSessions: 10, modelRequests: 0 })
  await richText(restarted)
  const upload = 'Benign uploaded source marker 773.\n'
  await restarted.locator('input[type="file"]').first().setInputFiles({ name: 'context-note.txt', mimeType: 'text/plain', buffer: Buffer.from(upload) })
  await restarted.waitForFunction(() => window.app.getToolView('richtextinject')?.richTextInjectUploadedSource?.includes('marker 773'))
  await restarted.evaluate(() => window.IceSkyRuntime.flush())
  const savedFile = await harness.context.request.get(`${harness.origin}/rainy/icesky/state?scope=${encodeURIComponent(`session:${ids[0]}`)}`)
  assert(savedFile.ok())
  const uploadedDraft = await savedFile.json()
  const uploadedTool = uploadedDraft.data.tools.richtextinject
  assert(!Object.hasOwn(uploadedTool.fields, 'richTextInjectUploadedSource'))
  assert(Object.values(uploadedTool.files).some(file => file.name === 'context-note.txt' && file.type === 'text/plain'))
  await record('uploaded txt retains metadata and excludes readonly raw source', { fields: Object.keys(uploadedTool.fields), files: uploadedTool.files })
  await page.reload()
  const afterFileReload = await chat(ids[0])
  await richText(afterFileReload)
  await afterFileReload.locator('.rainy-file-restore').waitFor({ state: 'visible' })
  assert.equal(await afterFileReload.locator('input[type="file"]').first().evaluate(input => input.files.length), 0)
  await page.screenshot({ path: resolve(output, 'file-reselect.png') })
  await record('reload requires selected file to be reselected')
  await page.locator('[data-rainy-rail]').getByRole('button', { name: '设置', exact: true }).click()
  const settings = page.getByRole('dialog', { name: '设置', exact: true })
  await settings.getByRole('button', { name: '深色', exact: true }).click()
  await afterFileReload.waitForFunction(() => document.documentElement.style.colorScheme === 'dark')
  await page.keyboard.press('Escape')
  await settings.waitFor({ state: 'hidden' })
  await afterFileReload.waitForFunction(() => getComputedStyle(document.querySelector('.rainy-file-restore')).backgroundColor
    === getComputedStyle(document.body).getPropertyValue('--rainy-panel').trim())
  report.darkContrast = await afterFileReload.evaluate(() => {
    const luminance = color => {
      const channels = color.match(/[\d.]+/g).slice(0, 3).map(Number).map(value => {
        const normalized = value / 255
        return normalized <= 0.04045 ? normalized / 12.92 : ((normalized + 0.055) / 1.055) ** 2.4
      })
      return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722
    }
    return ['.rainy-file-restore', '.section-header-card.workbench-intro-card', '.action-button:not(:disabled)', '.tool-primary-btn', '.injectlab-preview-surface'].map(selector => {
      const style = getComputedStyle(document.querySelector(selector))
      const foreground = luminance(style.color), background = luminance(style.backgroundColor)
      return { selector, color: style.color, background: style.backgroundColor, ratio: (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05) }
    })
  })
  assert(report.darkContrast.every(value => value.ratio >= 4.5))
  report.darkComponents = await afterFileReload.evaluate(() => [...document.querySelectorAll('*')].filter(element => {
    const bounds = element.getBoundingClientRect()
    return bounds.width > 20 && bounds.height > 8 && getComputedStyle(element).backgroundColor === 'rgb(255, 255, 255)'
  }).slice(0, 15).map(element => ({ tag: element.tagName, classes: element.className,
    color: getComputedStyle(element).color, background: getComputedStyle(element).backgroundColor,
    panel: getComputedStyle(document.body).getPropertyValue('--rainy-panel') })))
  await page.screenshot({ path: resolve(output, 'dark.png') })
  await record('host dark appearance reaches retained frame')
  await page.locator('[data-rainy-rail]').getByRole('button', { name: '设置', exact: true }).click()
  await settings.getByRole('button', { name: '浅色', exact: true }).click()
  await afterFileReload.waitForFunction(() => document.documentElement.style.colorScheme === 'light')
  const previousFont = await afterFileReload.evaluate(() => parseFloat(document.documentElement.style.getPropertyValue('--rainy-font-size')))
  const previousCodeFont = await afterFileReload.evaluate(() => parseFloat(document.documentElement.style.getPropertyValue('--rainy-code-font-size')))
  await settings.getByRole('button', { name: '增大字号', exact: true }).click()
  await afterFileReload.waitForFunction(previous => parseFloat(document.documentElement.style.getPropertyValue('--rainy-font-size')) === previous + 1, previousFont)
  await settings.getByRole('button', { name: '增大代码字号', exact: true }).click()
  await afterFileReload.waitForFunction(previous => parseFloat(document.documentElement.style.getPropertyValue('--rainy-code-font-size')) === previous + 1, previousCodeFont)
  await page.keyboard.press('Escape')
  await settings.waitFor({ state: 'hidden' })
  await page.screenshot({ path: resolve(output, 'light-font.png') })
  await record('host light appearance and both font increases reach retained frame', { previousFont, currentFont: previousFont + 1, previousCodeFont, currentCodeFont: previousCodeFont + 1 })
  await page.setViewportSize({ width: 950, height: 760 })
  const toolbar = []
  for (const name of ['搜索工具', '全部工具']) toolbar.push(await afterFileReload.getByRole('button', { name, exact: true }).evaluate(button => ({ text: button.textContent.trim(), classes: button.className, width: button.getBoundingClientRect().width, client: button.clientWidth, scroll: button.scrollWidth })))
  assert.equal(toolbar.length, 2)
  assert(toolbar.every(button => button.width > 0 && button.scroll <= button.client))
  await page.screenshot({ path: resolve(output, 'narrow-950.png') })
  await record('narrow main window captured', { toolbar })
  assert.deepEqual(report.errors, [])
  report.blockedExternalHosts = [...new Set(harness.blocked)]
  await record('main UI context smoke complete')
} catch (error) {
  report.failure = error.stack
  if (page && !page.isClosed()) {
    report.pageText = (await page.locator('body').innerText().catch(() => '')).slice(0, 1500)
    await page.screenshot({ path: resolve(output, 'failure.png') }).catch(() => {})
  }
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n')
  console.error(JSON.stringify({ failure: error.message, output }))
  process.exitCode = 1
} finally { await harness?.stop() }
