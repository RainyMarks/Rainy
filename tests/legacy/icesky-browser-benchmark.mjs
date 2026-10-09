/** Measure the bundled IceSky page through a private real Rainy profile with synthetic text. */
import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const argumentsByName = new Map(process.argv.slice(2).map((value, index, all) => [value, all[index + 1]]))
const runtime = argumentsByName.get('--runtime')
const output = argumentsByName.get('--output')
const playwrightPath = argumentsByName.get('--playwright')
if (!runtime || !output || !playwrightPath) throw new Error('Expected --runtime, --output and --playwright paths')
const { chromium } = await import(pathToFileURL(resolve(playwrightPath)).href)
const samples = Number(argumentsByName.get('--samples') ?? 10)
const runId = `icesky-ui-${Date.now()}-${process.pid}`
const child = spawn('wsl.exe', ['-d', 'Ubuntu', '--exec', 'env', `RAINY_HOME=/tmp/${runId}`, 'RAINY_CONFIGURE_DEEPSEEK=0',
  `${runtime}/node/bin/node`, '--expose-internals', `${runtime}/app/lib/host.js`], { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] })
const exited = new Promise(resolveExit => child.once('exit', resolveExit))
let diagnostic = ''
child.stderr.on('data', chunk => { diagnostic = (diagnostic + String(chunk)).slice(-6000) })
const lines = createInterface({ input: child.stdout })
let browser
try {
  const ready = await new Promise((resolveReady, reject) => {
    const deadline = setTimeout(() => reject(new Error(`Host readiness timed out: ${diagnostic}`)), 60000)
    child.once('error', reject)
    child.once('exit', code => { clearTimeout(deadline); reject(new Error(`Host exited ${code}: ${diagnostic}`)) })
    lines.on('line', line => {
      if (!line.startsWith('RAINY_CONTROL ')) return
      const message = JSON.parse(line.slice(14))
      if (message.type === 'ready') { clearTimeout(deadline); resolveReady(message) }
      else if (message.type === 'fatal') { clearTimeout(deadline); reject(new Error(message.message)) }
    })
  })
  browser = await chromium.launch({ channel: 'msedge', headless: true })
  const origin = new URL(ready.url).origin
  const report = { kind: 'icesky-browser-local-overhead', runtime, browser: browser.version(), samples,
    excluded: ['model requests', 'external network latency', 'whole desktop startup'], cold: [], warm: [], text: [], errors: [] }
  async function context() {
    const result = await browser.newContext({ viewport: { width: 900, height: 1000 } })
    await result.route('**/*', route => new URL(route.request().url()).origin === origin ? route.continue() : route.abort())
    await result.request.get(ready.url)
    await result.addInitScript(() => {
      window.__iceMetrics = { scans: 0, longTasks: [] }
      new PerformanceObserver(list => window.__iceMetrics.longTasks.push(...list.getEntries().map(entry => entry.duration))).observe({ type: 'longtask', buffered: true })
      let localization
      Object.defineProperty(window, 'LocalizationUtils', { configurable: true, get: () => localization, set(value) {
        localization = value
        if (typeof value?.localizeDom !== 'function') return
        const original = value.localizeDom
        value.localizeDom = function (root, ...args) {
          if (root === document.body) window.__iceMetrics.scans++
          return original.call(this, root, ...args)
        }
      } })
    })
    return result
  }
  async function open(page) {
    const errors = []
    page.on('pageerror', error => errors.push(error.message))
    const start = performance.now()
    await page.goto(`${origin}/rainy/icesky/index.html?embed=rainy&benchmark=1`)
    await page.waitForFunction(() => window.app && !window.app.toolLoading && document.querySelector('#app')?.getAttribute('v-cloak') === null)
    await page.evaluate(() => new Promise(resolveFrame => requestAnimationFrame(() => requestAnimationFrame(resolveFrame))))
    const elapsedMs = performance.now() - start
    const values = await page.evaluate(() => ({ ...window.__iceMetrics,
      resources: performance.getEntriesByType('resource').filter(entry => entry.name.startsWith(location.origin)).length,
      transferredBytes: performance.getEntriesByType('resource').reduce((total, entry) => total + (entry.transferSize ?? 0), 0),
      domNodes: document.getElementsByTagName('*').length,
      tools: (window.app.registeredTools ?? []).filter(tool => !tool.hidden).length,
    }))
    report.errors.push(...errors)
    return { elapsedMs, ...values }
  }
  for (let index = 0; index < samples; index++) {
    const current = await context()
    try { report.cold.push(await open(await current.newPage())) } finally { await current.close() }
  }
  const current = await context()
  try {
    const page = await current.newPage()
    await open(page)
    for (let index = 0; index < samples; index++) report.warm.push(await open(page))
    for (const bytes of [2048, 65536, 102400, 1048576]) {
      for (let index = 0; index < samples; index++) {
        await open(page)
        await page.evaluate(() => window.app.switchToTab('transforms'))
        const input = page.locator('#transform-input-single:visible, #transform-input:visible').first()
        await input.waitFor({ state: 'visible' })
        await page.evaluate(async () => {
          const view = typeof window.app.getToolView === 'function' ? window.app.getToolView('transforms') : window.app
          if (typeof view.ensureTransformAssetsLoaded === 'function') await view.ensureTransformAssetsLoaded()
          await view.$nextTick()
        })
        await input.fill('')
        await page.waitForTimeout(200)
        await page.evaluate(() => { window.__iceMetrics.scans = 0; window.__iceMetrics.longTasks = [] })
        const text = 'Ordinary example text. '.repeat(Math.ceil(bytes / 23)).slice(0, bytes)
        const start = performance.now()
        await input.fill(text)
        await page.evaluate(() => new Promise(resolveFrame => requestAnimationFrame(() => requestAnimationFrame(resolveFrame))))
        report.text.push({ bytes: Buffer.byteLength(text), inputToPaintMs: performance.now() - start,
          ...await page.evaluate(() => ({ ...window.__iceMetrics, domNodes: document.getElementsByTagName('*').length })) })
      }
    }
  } finally { await current.close() }
  const summary = values => {
    const sorted = [...values].sort((a, b) => a - b)
    const middle = Math.floor(sorted.length / 2)
    const median = sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2
    return { median, p95: sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * .95) - 1)] }
  }
  report.summary = { coldMs: summary(report.cold.map(value => value.elapsedMs)), warmMs: summary(report.warm.map(value => value.elapsedMs)),
    text: Object.fromEntries([...new Set(report.text.map(value => value.bytes))].map(bytes => [bytes, summary(report.text.filter(value => value.bytes === bytes).map(value => value.inputToPaintMs))])) }
  report.errors = [...new Set(report.errors)]
  await mkdir(resolve(output, '..'), { recursive: true })
  await writeFile(output, JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify({ output, summary: report.summary, errors: report.errors }))
} finally {
  await browser?.close()
  lines.close()
  if (child.stdin.writable) child.stdin.end('{"type":"stop"}\n')
  const timeout = setTimeout(() => child.kill(), 10000)
  await exited
  clearTimeout(timeout)
}
