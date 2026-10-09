/** Headless acceptance of real installed catalog data and offline web tools; no desktop automation. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { randomUUID } from 'node:crypto'
import { NativeToolsLibrary, parseNativeFavorites, parseNativeLaunch } from '../src/native-tools.ts'
import { serveNativeTool } from '../src/native-tool-web.ts'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'
import { nativeToolIds } from '../../../packages/client/ui-rainy/src/native-tools-protocol.ts'

const runtime = process.argv[2]
const installRoot = resolve(process.argv[3] ?? 'apps/rainy-desktop/validation/native-tools-clean-install')
assert.ok(runtime, 'Pass the prepared Linux runtime path')
const output = resolve(process.argv[4] ?? 'apps/rainy-desktop/validation/native-tools')
const userData = resolve('.artifacts', `native-tools-browser-${randomUUID()}`)
await mkdir(output, { recursive: true })
await mkdir(userData, { recursive: true })
const harness = await openWorkbenchHarness({ runtime })
const opened = []
const errors = []
const externalRequests = []
const checks = []
const report = { mode: 'headless browser with real catalog and launch service; no native GUI automation', runtime, installRoot, checks, errors, externalRequests }
const library = new NativeToolsLibrary({ installRoot, userData, start: async invocation => {
  assert.equal(invocation.kind, 'web', 'This acceptance run never opens desktop tools or consoles')
  const server = await serveNativeTool(invocation)
  const context = await harness.browser.newContext()
  await context.route('**/*', route => {
    if (new URL(route.request().url()).origin === server.origin) return route.continue()
    externalRequests.push({ tool: invocation.id, host: new URL(route.request().url()).hostname })
    return route.abort()
  })
  const page = await context.newPage()
  page.on('pageerror', error => { errors.push({ tool: invocation.id, error: error.message }) })
  opened.push({ id: invocation.id, page, context, server })
  await page.goto(server.url, { waitUntil: 'load' })
} })

try {
  const catalog = await library.listTools()
  assert.deepEqual(catalog.tools.map(tool => tool.id).sort(), [...nativeToolIds].sort())
  assert.equal(catalog.tools.filter(tool => tool.status !== 'ready').length, 0)
  checks.push({ name: `${nativeToolIds.length} real tool entries and contained runtime paths`, passed: true })
  await harness.context.exposeBinding('__nativeCatalogRequest', async (_source, operation, value) => {
    if (operation === 'list') return library.listTools()
    if (operation === 'favorites') return library.setFavorites(parseNativeFavorites(value))
    if (operation === 'launch') { const request = parseNativeLaunch(value); return library.launchTool(request.id, request.variant) }
    throw new Error('Unknown catalog operation')
  })
  await harness.context.addInitScript(() => {
    window.__RAINY_TOOLS__ = {
      checkToolUpdates: async () => ({ phase: 'current', version: '1.0.0', error: '' }),
      getDownloadState: async () => ({ phase: 'idle', completedBytes: 0, totalBytes: 2318669038, error: '' }),
      downloadTools: async () => {}, cancelDownload: async () => {}, onDownloadProgress: () => () => {},
      listTools: () => window.__nativeCatalogRequest('list'),
      setFavorites: ids => window.__nativeCatalogRequest('favorites', ids),
      launchTool: (id, variant) => window.__nativeCatalogRequest('launch', { id, ...variant === undefined ? {} : { variant } }),
    }
  })
  const page = await harness.context.newPage()
  page.on('pageerror', error => { errors.push({ tool: 'catalog', error: error.message }) })
  await page.goto(harness.origin)
  await page.getByText('Develop by NCUCyberBase', { exact: true }).waitFor({ timeout: 60000 })
  assert.equal(await page.title(), 'RainyAgent')
  await page.screenshot({ path: resolve(output, 'hero.png') })
  checks.push({ name: 'packaged profile displays RainyAgent and the NCUCyberBase attribution', passed: true })
  await page.getByRole('button', { name: /^(CTF 工具|CTF tools)$/ }).click({ timeout: 60000 })
  await page.locator('[data-tool-id]').first().waitFor({ timeout: 60000 })
  assert.equal(await page.locator('[data-tool-id]').count(), nativeToolIds.length)
  assert.equal(await page.locator('[data-rainy-ctf-workbench] iframe').count(), 0)
  await page.screenshot({ path: resolve(output, 'catalog.png') })
  await page.getByRole('textbox', { name: /搜索名称或用途|Search names or uses/ }).fill('ffprobe')
  assert.equal(await page.locator('[data-tool-id]').count(), 1)
  assert.equal(await page.locator('[data-tool-id]').getAttribute('data-tool-id'), 'ffmpeg')
  await page.getByRole('textbox', { name: /搜索名称或用途|Search names or uses/ }).fill('')
  checks.push({ name: 'real profile catalog searches the packaged tools and leaves IceSky lazy', passed: true })
  await page.locator('[data-tool-id="cyberchef"]').getByRole('button', { name: /^(收藏|Favorite) CyberChef$/ }).click()
  await page.waitForFunction(() => document.querySelector('[data-tool-id="cyberchef"] button[aria-pressed="true"]'))
  assert.deepEqual((await library.listTools()).preferences.favorites, ['cyberchef'])
  await page.reload()
  await page.getByRole('button', { name: /^(CTF 工具|CTF tools)$/ }).click()
  await page.locator('[data-tool-id="cyberchef"]').getByRole('button', { name: /^(取消收藏|Unfavorite) CyberChef$/ }).waitFor()
  checks.push({ name: 'favorites survive a renderer reload through real file storage', passed: true })
  await page.locator('[data-tool-id="cyberchef"]').getByRole('button', { name: /^(打开|Open) CyberChef$/ }).click()
  await page.getByText(/已发送 CyberChef 的启动请求|Launch request sent for CyberChef/, { exact: true }).waitFor({ timeout: 60000 })
  const chef = opened.find(item => item.id === 'cyberchef').page
  await chef.waitForFunction(() => document.querySelector('#input-text') || document.querySelector('.cm-content'), null, { timeout: 60000 })
  const structure = await chef.evaluate(() => ({ title: document.title, inputs: [...document.querySelectorAll('textarea,input')].map(node => ({ tag: node.tagName, id: node.id, type: node.type })).slice(0, 60) }))
  report.cyberchef = structure
  await chef.goto(`${chef.url().split('#')[0]}#recipe=To_Base64('A-Za-z0-9%2B/%3D')&input=UmFpbnkgSGVsbG8`, { waitUntil: 'load' })
  await chef.waitForFunction(() => document.querySelector('#output-text')?.value === 'UmFpbnkgSGVsbG8=' || document.body.innerText.includes('UmFpbnkgSGVsbG8='), null, { timeout: 60000 })
  checks.push({ name: 'CyberChef performs a local Base64 conversion in the packaged web app', passed: true })
  assert.equal(await chef.evaluate(() => typeof window.__RAINY_TOOLS__), 'undefined')
  assert.equal(await chef.evaluate(() => typeof window.require), 'undefined')
  checks.push({ name: 'offline tool page receives no catalog or Node bridge', passed: true })

  for (const id of ['qrazybox', 'image-lsb-viewer']) {
    const result = await library.launchTool(id)
    assert.equal(result.ok, true, result.error)
    const tool = opened.find(item => item.id === id).page
    report[id] = await tool.evaluate(() => ({ title: document.title, body: document.body.innerText.slice(0, 1500),
      inputs: [...document.querySelectorAll('input,textarea,canvas')].map(node => ({ tag: node.tagName, id: node.id, type: node.type })) }))
    if (id === 'qrazybox') {
      await tool.locator('#home-new').click()
      await tool.locator('#new-btn-new').click()
      assert.equal(await tool.locator('#qr-table tr').count(), 21)
      assert.equal(await tool.locator('#qr-table td').count(), 441)
      await tool.locator('#btn-version-plus').click()
      assert.equal(await tool.locator('#qr-table tr').count(), 25)
      assert.equal(await tool.locator('#qr-table td').count(), 625)
      checks.push({ name: 'QRazyBox creates and resizes a local QR grid', passed: true })
    } else {
      const png = await tool.evaluate(async () => {
        const canvas = new OffscreenCanvas(2, 2)
        const context = canvas.getContext('2d')
        context.fillStyle = 'rgb(1, 2, 3)'
        context.fillRect(0, 0, 2, 2)
        return [...new Uint8Array(await (await canvas.convertToBlob({ type: 'image/png' })).arrayBuffer())]
      })
      await tool.locator('input.imgFileSelector').setInputFiles({ name: 'fixture.png', mimeType: 'image/png', buffer: Buffer.from(png) })
      await tool.waitForFunction(() => document.querySelector('canvas.finalImage')?.width === 2)
      const firstPixel = () => tool.evaluate(() => [...document.querySelector('canvas.finalImage').getContext('2d').getImageData(0, 0, 1, 1).data])
      assert.deepEqual(await firstPixel(), [1, 2, 3, 255])
      await tool.locator('.switchRight').click()
      assert.deepEqual(await firstPixel(), [254, 253, 252, 255])
      checks.push({ name: 'ImageLSBViewer loads a local PNG and applies its image transformation', passed: true })
    }
  }
  assert.deepEqual(errors, [])
  assert.deepEqual(externalRequests, [])
  await writeFile(resolve(output, 'browser.json'), `${JSON.stringify(report, null, 2)}\n`)
  process.stdout.write(JSON.stringify({ checks: checks.length, errors, externalRequests, report: resolve(output, 'browser.json') }) + '\n')
} catch (error) {
  report.failure = error instanceof Error ? error.message : String(error)
  await writeFile(resolve(output, 'browser.json'), `${JSON.stringify(report, null, 2)}\n`)
  throw error
} finally {
  await Promise.all(opened.map(async item => { await item.context.close(); await item.server.close() }))
  await harness.stop()
}
