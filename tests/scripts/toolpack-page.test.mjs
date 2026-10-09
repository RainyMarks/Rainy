/** The offline tool maintenance page rendered in happy-dom; the native bridge is a test fixture. */
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { Window } from 'happy-dom'

const PAGE = new URL('../../src/setup/toolpack.html', import.meta.url)
const HTML = readFileSync(PAGE, 'utf8')
const SCRIPT = readFileSync(new URL('../../src/setup/toolpack.js', import.meta.url), 'utf8')
const STYLES = readFileSync(new URL('../../src/setup/toolpack.css', import.meta.url), 'utf8')
const INITIAL = { phase: 'prepare', message: '正在准备离线安装', completedBytes: 0, totalBytes: 0, cancelling: false }
const settle = () => new Promise(resolve => { setImmediate(resolve) })

/**
 * Load the page with a fixture bridge and run its script.
 * @returns page accessors, the bridge state and an emitter for progress snapshots.
 */
async function fixture(owner, { initial = INITIAL, deferredInitial = false, missingBridge = false } = {}) {
  const window = new Window({ url: PAGE.href, settings: { disableJavaScriptFileLoading: true, disableCSSFileLoading: true } })
  owner.after(async () => { await window.happyDOM.close() })
  window.document.write(HTML.replace(/<script\b[^>]*><\/script>/u, ''))
  const listeners = new Set()
  const state = { cancelCalls: 0, unsubscribed: 0 }
  const control = {}
  if (!missingBridge) {
    window.__RAINY_TOOLPACK__ = {
      getProgress: () => deferredInitial ? new Promise(resolve => { control.resolveInitial = () => resolve(initial) }) : Promise.resolve(initial),
      cancel: () => {
        state.cancelCalls++
        return new Promise((resolve, reject) => {
          control.resolveCancel = resolve
          control.rejectCancel = () => reject(new Error('取消请求未能发送，请重试'))
        })
      },
      onProgress(listener) {
        listeners.add(listener)
        return () => { state.unsubscribed++; listeners.delete(listener) }
      },
    }
  }
  new Function('window', 'document', SCRIPT)(window, window.document)
  await settle()
  const byId = id => window.document.getElementById(id)
  const text = id => byId(id).textContent
  const emit = async snapshot => { for (const listener of listeners) listener({ ...INITIAL, ...snapshot }); await settle() }
  const resolve = async name => { control[name]?.(); await settle() }
  return { window, byId, text, emit, resolve, state }
}

test('renders stage bytes and file names without treating disk-space requirements as completed work', async (owner) => {
  const { window, byId, text, emit } = await fixture(owner)
  assert.equal(text('heading'), '安装离线工具')
  assert.equal(text('cancel'), '取消安装')
  assert.equal(byId('cancel').disabled, false)
  assert.equal(byId('progress').getAttribute('value'), null)
  await emit({ phase: 'extracting', message: '正在解包工具', completedBytes: 1024, totalBytes: 4096, currentPath: 'tools/示例 tool/<test>.bin' })
  assert.equal(text('phase'), '解包工具')
  assert.equal(text('percent'), '25%')
  assert.equal(text('bytes'), '1 KiB / 4 KiB')
  assert.equal(text('current-path'), 'tools/示例 tool/<test>.bin')
  assert.equal(byId('current-path').querySelector('test'), null)
  assert.equal(byId('progress').getAttribute('aria-valuetext'), '当前阶段 25%，1 KiB / 4 KiB')
  await emit({ phase: 'checking-space', message: '正在检查磁盘空间', completedBytes: 0, totalBytes: 10 * 1024 ** 3 })
  assert.equal(text('phase'), '检查磁盘空间')
  assert.equal(byId('progress').getAttribute('value'), null)
  assert.equal(text('percent'), '')
  assert.equal(text('bytes'), '')
  assert.equal(byId('current-path').hidden, true)
  for (const element of window.document.querySelectorAll('[src], [href]')) {
    assert.doesNotMatch(element.getAttribute('src') ?? element.getAttribute('href'), /^[a-z][a-z0-9+.-]*:|^\/\//iu)
  }
  const policy = window.document.querySelector('meta[http-equiv="Content-Security-Policy"]').getAttribute('content')
  assert.match(policy, /script-src 'self'/)
  assert.match(policy, /style-src 'self'/)
  assert.doesNotMatch(policy, /unsafe-inline|https?:/)
})

test('keeps cancellation pending until a terminal update and never closes or navigates the page', async (owner) => {
  const { window, byId, text, emit, resolve, state } = await fixture(owner)
  const before = window.location.href
  byId('cancel').click()
  byId('cancel').click()
  await settle()
  assert.equal(state.cancelCalls, 1)
  assert.equal(byId('cancel').disabled, true)
  assert.equal(text('cancel-state'), '正在安全停止，请稍候')
  await resolve('resolveCancel')
  assert.equal(byId('cancel').disabled, true)
  await emit({ phase: 'rolling-back', message: '正在恢复原有工具', cancelling: true })
  assert.equal(text('phase'), '恢复原有工具')
  assert.equal(text('cancel-state'), '正在安全停止，请稍候')
  await emit({ phase: 'cancelled', message: '已取消，原有工具保持可用' })
  assert.equal(text('phase'), '已取消安装')
  assert.equal(byId('cancel').disabled, true)
  assert.equal(text('cancel-state'), '')
  assert.equal(byId('progress').hidden, true)
  assert.equal(window.location.href, before)
})

test('allows retry after an undelivered cancellation request and disables cancellation after completion', async (owner) => {
  const { byId, text, emit, resolve, state } = await fixture(owner)
  byId('cancel').click()
  await resolve('rejectCancel')
  assert.equal(byId('cancel-state').hasAttribute('data-error'), true)
  assert.equal(text('cancel-state'), '取消请求未能发送，请重试')
  assert.equal(byId('cancel').disabled, false)
  byId('cancel').click()
  await settle()
  assert.equal(state.cancelCalls, 2)
  await emit({ phase: 'complete', message: '离线工具安装完成', completedBytes: 4096, totalBytes: 4096 })
  assert.equal(text('phase'), '安装完成')
  assert.equal(text('percent'), '100%')
  assert.equal(byId('cancel').disabled, true)
  await resolve('rejectCancel')
  assert.equal(text('cancel-state'), '')
})

test('retains newer events over a late initial snapshot and disposes the subscription once', async (owner) => {
  const { window, text, emit, resolve, state } = await fixture(owner, { deferredInitial: true })
  await emit({ phase: 'checking-media', message: '正在校验分卷', completedBytes: 5, totalBytes: 10 })
  await resolve('resolveInitial')
  assert.equal(text('phase'), '校验安装文件')
  assert.equal(text('percent'), '50%')
  window.dispatchEvent(new window.Event('pagehide'))
  window.dispatchEvent(new window.Event('unload'))
  assert.equal(state.unsubscribed, 1)
  await emit({ phase: 'complete', message: 'late event', completedBytes: 10, totalBytes: 10 })
  assert.equal(text('phase'), '校验安装文件')
})

test('defines a dark palette and stops the waiting animation for reduced motion', () => {
  assert.match(STYLES, /@media \(prefers-color-scheme: dark\) \{\s*:root \{[^}]*--toolpack-track:/u)
  assert.match(STYLES, /@media \(prefers-reduced-motion: reduce\) \{\s*progress:not\(\[value\]\) \{ animation: none; \}/u)
  assert.match(STYLES, /\.current-path \{[^}]*overflow-wrap: anywhere;/u)
})

test('shows a local connection error without an active cancellation control when no preload is present', async (owner) => {
  const { byId, text } = await fixture(owner, { missingBridge: true })
  assert.equal(text('phase'), '安装服务未连接')
  assert.equal(byId('cancel').disabled, true)
  assert.equal(byId('progress').hidden, true)
})
