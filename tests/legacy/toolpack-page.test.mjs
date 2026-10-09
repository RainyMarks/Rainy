/** Headless local-file checks for the maintenance page; native work is an explicit API fixture. */
import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { before, after, test } from 'node:test'

const require = createRequire(new URL('../../web/package.json', import.meta.url))
const { chromium } = require('playwright')
const PAGE = new URL('../src/setup/toolpack.html', import.meta.url).href
const INITIAL = { phase: 'prepare', message: '正在准备离线安装', completedBytes: 0, totalBytes: 0, cancelling: false }
let browser

before(async () => { browser = await chromium.launch({ headless: true }) })
after(async () => { await browser?.close() })

async function fixture(owner, { initial = INITIAL, deferredInitial = false, missingBridge = false } = {}) {
  const context = await browser.newContext({ viewport: { width: 720, height: 560 }, locale: 'zh-CN' })
  owner.after(async () => { await context.close() })
  const page = await context.newPage()
  const requests = []
  page.on('request', request => { requests.push(request.url()) })
  await page.addInitScript(({ initial, deferredInitial, missingBridge }) => {
    const listeners = new Set()
    let finishInitial
    let finishCancel
    let rejectCancel
    const state = { cancelCalls: 0, unsubscribed: 0 }
    window.__TOOLPACK_FIXTURE__ = {
      state,
      emit(snapshot) { for (const listener of listeners) listener(snapshot) },
      resolveInitial() { finishInitial?.(initial) },
      resolveCancel() { finishCancel?.() },
      rejectCancel() { rejectCancel?.(new Error('取消请求未能发送，请重试')) },
    }
    if (missingBridge) return
    window.__RAINY_TOOLPACK__ = {
      getProgress: () => deferredInitial ? new Promise(resolve => { finishInitial = resolve }) : Promise.resolve(initial),
      cancel: () => {
        state.cancelCalls++
        return new Promise((resolve, reject) => { finishCancel = resolve; rejectCancel = reject })
      },
      onProgress(listener) {
        listeners.add(listener)
        return () => { state.unsubscribed++; listeners.delete(listener) }
      },
    }
  }, { initial, deferredInitial, missingBridge })
  await page.goto(PAGE)
  await page.getByRole('heading', { name: '安装离线工具' }).waitFor({ state: 'visible' })
  const emit = async snapshot => {
    await page.evaluate(value => { window.__TOOLPACK_FIXTURE__.emit(value) }, { ...INITIAL, ...snapshot })
  }
  return { page, emit, requests }
}

test('renders stage bytes and file names without treating disk-space requirements as completed work', async (owner) => {
  const { page, emit, requests } = await fixture(owner)
  await page.getByRole('button', { name: '取消安装' }).waitFor({ state: 'visible' })
  assert.equal(await page.locator('#progress').getAttribute('value'), null)
  await emit({ phase: 'extracting', message: '正在解包工具', completedBytes: 1024, totalBytes: 4096,
    currentPath: 'tools/示例 tool/<test>.bin' })
  assert.equal(await page.locator('#phase').textContent(), '解包工具')
  assert.equal(await page.locator('#percent').textContent(), '25%')
  assert.equal(await page.locator('#bytes').textContent(), '1 KiB / 4 KiB')
  assert.equal(await page.locator('#current-path').textContent(), 'tools/示例 tool/<test>.bin')
  assert.equal(await page.locator('#current-path test').count(), 0)
  assert.equal(await page.locator('#progress').getAttribute('aria-valuetext'), '当前阶段 25%，1 KiB / 4 KiB')
  await emit({ phase: 'checking-space', message: '正在检查磁盘空间', completedBytes: 0, totalBytes: 10 * 1024 ** 3 })
  assert.equal(await page.locator('#phase').textContent(), '检查磁盘空间')
  assert.equal(await page.locator('#progress').getAttribute('value'), null)
  assert.equal(await page.locator('#percent').textContent(), '')
  assert.equal(await page.locator('#bytes').textContent(), '')
  assert.equal(await page.locator('#current-path').isHidden(), true)
  assert.ok(requests.every(url => new URL(url).protocol === 'file:'))
  const policy = await page.locator('meta[http-equiv="Content-Security-Policy"]').getAttribute('content')
  assert.match(policy, /script-src 'self'/)
  assert.match(policy, /style-src 'self'/)
  assert.doesNotMatch(policy, /unsafe-inline|https?:/)
})

test('keeps cancellation pending until a terminal update and never closes or navigates the page', async (owner) => {
  const { page, emit } = await fixture(owner)
  const before = page.url()
  await page.getByRole('button', { name: '取消安装' }).click()
  await page.evaluate(() => { document.getElementById('cancel').click() })
  assert.equal(await page.evaluate(() => window.__TOOLPACK_FIXTURE__.state.cancelCalls), 1)
  assert.equal(await page.locator('#cancel').isDisabled(), true)
  assert.equal(await page.locator('#cancel-state').textContent(), '正在安全停止，请稍候')
  await page.evaluate(async () => { window.__TOOLPACK_FIXTURE__.resolveCancel(); await Promise.resolve() })
  assert.equal(await page.locator('#cancel').isDisabled(), true)
  await emit({ phase: 'rolling-back', message: '正在恢复原有工具', cancelling: true })
  assert.equal(await page.locator('#phase').textContent(), '恢复原有工具')
  assert.equal(await page.locator('#cancel-state').textContent(), '正在安全停止，请稍候')
  await emit({ phase: 'cancelled', message: '已取消，原有工具保持可用' })
  assert.equal(await page.locator('#phase').textContent(), '已取消安装')
  assert.equal(await page.locator('#cancel').isDisabled(), true)
  assert.equal(await page.locator('#cancel-state').textContent(), '')
  assert.equal(await page.locator('#progress').isHidden(), true)
  assert.equal(page.isClosed(), false)
  assert.equal(page.url(), before)
})

test('allows retry after an undelivered cancellation request and disables cancellation after completion', async (owner) => {
  const { page, emit } = await fixture(owner)
  await page.getByRole('button', { name: '取消安装' }).click()
  await page.evaluate(async () => { window.__TOOLPACK_FIXTURE__.rejectCancel(); await Promise.resolve() })
  await page.locator('#cancel-state[data-error]').waitFor({ state: 'visible' })
  assert.equal(await page.locator('#cancel-state').textContent(), '取消请求未能发送，请重试')
  assert.equal(await page.locator('#cancel').isDisabled(), false)
  await page.getByRole('button', { name: '取消安装' }).click()
  assert.equal(await page.evaluate(() => window.__TOOLPACK_FIXTURE__.state.cancelCalls), 2)
  await emit({ phase: 'complete', message: '离线工具安装完成', completedBytes: 4096, totalBytes: 4096 })
  assert.equal(await page.locator('#phase').textContent(), '安装完成')
  assert.equal(await page.locator('#percent').textContent(), '100%')
  assert.equal(await page.locator('#cancel').isDisabled(), true)
  await page.evaluate(async () => { window.__TOOLPACK_FIXTURE__.rejectCancel(); await Promise.resolve() })
  assert.equal(await page.locator('#cancel-state').textContent(), '')
})

test('retains newer events over a late initial snapshot and disposes the subscription once', async (owner) => {
  const { page, emit } = await fixture(owner, { deferredInitial: true })
  await emit({ phase: 'checking-media', message: '正在校验分卷', completedBytes: 5, totalBytes: 10 })
  await page.evaluate(async () => { window.__TOOLPACK_FIXTURE__.resolveInitial(); await Promise.resolve() })
  assert.equal(await page.locator('#phase').textContent(), '校验安装文件')
  assert.equal(await page.locator('#percent').textContent(), '50%')
  await page.evaluate(() => {
    window.dispatchEvent(new Event('pagehide'))
    window.dispatchEvent(new Event('unload'))
  })
  assert.equal(await page.evaluate(() => window.__TOOLPACK_FIXTURE__.state.unsubscribed), 1)
  await emit({ phase: 'complete', message: 'late event', completedBytes: 10, totalBytes: 10 })
  assert.equal(await page.locator('#phase').textContent(), '校验安装文件')
})

test('changes the local palette across themes without narrow-view overflow and honors reduced motion', async (owner) => {
  const { page, emit } = await fixture(owner)
  await page.setViewportSize({ width: 400, height: 560 })
  await emit({ phase: 'switching', message: '正在切换到已校验的新工具版本', currentPath: `tools/中文 路径/${'a'.repeat(100)}.dll` })
  await page.emulateMedia({ colorScheme: 'light', reducedMotion: 'reduce' })
  const light = await page.evaluate(() => ({ background: getComputedStyle(document.documentElement).backgroundColor,
    text: getComputedStyle(document.documentElement).color, overflow: document.documentElement.scrollWidth > innerWidth,
    animation: getComputedStyle(document.getElementById('progress')).animationName }))
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' })
  const dark = await page.evaluate(() => ({ background: getComputedStyle(document.documentElement).backgroundColor,
    text: getComputedStyle(document.documentElement).color, overflow: document.documentElement.scrollWidth > innerWidth }))
  assert.notEqual(light.background, dark.background)
  assert.notEqual(light.text, dark.text)
  assert.equal(light.animation, 'none')
  assert.equal(light.overflow, false)
  assert.equal(dark.overflow, false)
})

test('shows a local connection error without an active cancellation control when no preload is present', async (owner) => {
  const { page } = await fixture(owner, { missingBridge: true })
  assert.equal(await page.locator('#phase').textContent(), '安装服务未连接')
  assert.equal(await page.locator('#cancel').isDisabled(), true)
  assert.equal(await page.locator('#progress').isHidden(), true)
})
