/** Measure the real sidebar and retained workbench frames with private Sessions and empty transform inputs. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const argumentsByName = new Map(process.argv.slice(2).map((value, index, values) => [value, values[index + 1]]))
const runtime = process.argv[2]
const label = argumentsByName.get('--label') ?? 'runtime'
const samples = Number(argumentsByName.get('--samples') ?? 10)
const switchesPerSample = Number(argumentsByName.get('--switches') ?? 10)
if (!runtime || !Number.isInteger(samples) || samples < 1 || !Number.isInteger(switchesPerSample) || switchesPerSample < 1) {
  throw new Error('Pass a private runtime directory with positive --samples and --switches counts')
}
const output = resolve('apps/rainy-desktop/validation/icesky-browser-retention', label)
await mkdir(output, { recursive: true })
const harness = await openWorkbenchHarness({ runtime, fixture: true })
const report = { kind: 'real-main-ui-workbench-retention', label, runtime, samples, switchesPerSample,
  startedAtUtc: new Date().toISOString(),
  mode: samples === 1 ? 'functional-smoke' : 'measurement', browser: harness.browser.version(),
  viewport: { width: 1380, height: 920 }, colorScheme: 'light', observations: [], errors: [], consoleErrors: [], blockedExternalHosts: [],
  methodology: {
    setup: 'One real profile with ten private durable Sessions and loopback model fixtures; every sample has a fresh browser context and page.',
    visits: 'Expand the actual workspace sidebar, select the first N Session rows, and open CTF tools through the actual activity button for each.',
    toolControl: 'Every visited workbench uses its shipped switch/open operation to select Text transforms. Its real ensureTransformAssetsLoaded Promise and Vue update finish before the visible textarea is filled with an empty string. Candidate runtime.flush is awaited before moving on or measuring; the legacy workbench has no Host draft flush API.',
    heap: 'HeapProfiler.collectGarbage followed by Runtime.getHeapUsage on the main-page CDP target, reporting its renderer isolate JavaScript heap and the returned backing/embedder sizes. This includes same-process frames; it is not browser/OS RSS.',
    dom: 'Element counts in the main document plus every same-origin iframe document, including hidden retained frames.',
    switch: 'Node-side monotonic time immediately before the sidebar click through selected-row plus visible ready workbench, two animation frames, and pending candidate draft flush. Includes Playwright click/wait overhead.',
    ready: 'Candidate additionally waits for its runtime context Session id. Legacy readiness uses the physical frame previously opened under that Session plus visible geometry and the real Vue app.',
    singleChat: 'The one-chat group reselects its existing sidebar row; it has no cross-chat transition and is labeled separately.',
    summary: 'Each group retains every raw footprint and selection sample. Median averages the two middle values for an even sample count; P95 uses nearest-rank ceil(.95*n)-1.',
  },
}

function summary(values) {
  const sorted = [...values].sort((a, b) => a - b)
  const middle = Math.floor(sorted.length / 2)
  return { median: sorted.length % 2 === 0 ? (sorted[middle - 1] + sorted[middle]) / 2 : sorted[middle],
    p95: sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * .95) - 1)] }
}

async function readyWorkbench(page, id, { establishOwner = false } = {}) {
  await page.waitForFunction(({ id, establishOwner }) => {
    const row = document.querySelector(`[data-row-key="session:${id}"]`)
    if (row?.getAttribute('aria-selected') !== 'true') return false
    const frames = Array.from(document.querySelectorAll('iframe')).filter(frame => {
      const rect = frame.getBoundingClientRect()
      const app = frame.contentWindow?.app
      return rect.width > 0 && rect.height > 0 && app?._isVue === true && app.$el?.isConnected && !app.toolLoading
    })
    if (frames.length !== 1) return false
    const frame = frames[0]
    const runtimeContext = frame.contentWindow.IceSkyRuntime?.context
    if (runtimeContext) {
      if (document.querySelector('[data-rainy-ctf-workbench] [aria-label="正在加载 CTF 工具"], [data-rainy-ctf-workbench] [aria-label="Loading CTF tools"]')) return false
      return runtimeContext.kind === 'session' && runtimeContext.id === id
        && frame.contentWindow.app.toolInstance?._iceSkyEpoch === frame.contentWindow.IceSkyRuntime.epoch
    }
    const owners = window.__retentionFrameOwners ??= new WeakMap()
    if (establishOwner && !owners.has(frame)) owners.set(frame, id)
    return owners.get(frame) === id
  }, { id, establishOwner })
  await page.evaluate(() => new Promise(resolvePaint => requestAnimationFrame(() => requestAnimationFrame(resolvePaint))))
}

async function emptyTransformWorkbench(page) {
  const handles = await page.locator('iframe').elementHandles()
  let visible
  for (const handle of handles) {
    const box = await handle.boundingBox()
    if (box?.width > 0 && box.height > 0) { visible = await handle.contentFrame(); break }
  }
  assert(visible, 'the selected chat must have its actual visible workbench')
  await visible.evaluate(async () => {
    if (typeof window.IceSkyRuntime?.openTool === 'function') await window.IceSkyRuntime.openTool('transforms')
    else if (typeof window.app.openTool === 'function') await window.app.openTool('transforms')
    else { window.app.switchToTab('transforms'); await window.app.$nextTick() }
  })
  await visible.waitForFunction(() => window.app.activeTab === 'transforms' && !window.app.toolLoading)
  await visible.evaluate(async () => {
    const view = window.app.getToolView?.('transforms') ?? window.app
    await view.ensureTransformAssetsLoaded()
    await view.$nextTick()
  })
  await visible.waitForFunction(() => {
    const view = window.app.getToolView?.('transforms') ?? window.app
    return view.transformAssetsReady && !view.transformAssetsLoading && view.transforms.length > 0
  })
  await visible.locator('#transform-input-single:visible, #transform-input:visible').first().fill('')
  await visible.evaluate(async () => { await window.app.$nextTick(); if (window.IceSkyRuntime?.flush) await window.IceSkyRuntime.flush() })
}

try {
  for (const chats of [1, 5, 10]) {
    for (let index = 0; index < samples; index++) {
      const context = await harness.browser.newContext({ viewport: report.viewport, colorScheme: report.colorScheme })
      await context.route('**/*', route => {
        const url = new URL(route.request().url())
        if (url.origin === harness.origin) return route.continue()
        report.blockedExternalHosts.push(url.hostname)
        return route.abort()
      })
      await context.request.get(harness.ready.url)
      const page = await context.newPage()
      page.on('pageerror', error => report.errors.push({ chats, index, message: error.message }))
      page.on('console', message => { if (message.type() === 'error') report.consoleErrors.push({ chats, index, message: message.text() }) })
      try {
        await page.goto(harness.origin)
        const expand = page.getByRole('button', { name: '展开其余 5 个会话' })
        await expand.click()
        for (let session = 1; session <= chats; session++) {
          const id = `icesky-ui-fixture-${session}`
          await page.locator(`[data-row-key="session:${id}"]`).click()
          await page.getByRole('button', { name: 'CTF 工具', exact: true }).click()
          await readyWorkbench(page, id, { establishOwner: true })
          await emptyTransformWorkbench(page)
        }
        const cdp = await context.newCDPSession(page)
        const executionContexts = []
        cdp.on('Runtime.executionContextCreated', value => {
          if (value.context.auxData?.isDefault) executionContexts.push({ frameId: value.context.auxData.frameId, origin: value.context.origin })
        })
        await cdp.send('Runtime.enable')
        await cdp.send('HeapProfiler.collectGarbage')
        const heap = await cdp.send('Runtime.getHeapUsage')
        const footprint = await page.evaluate(() => {
          const frames = Array.from(document.querySelectorAll('iframe')).map(frame => {
            const rect = frame.getBoundingClientRect()
            return { url: new URL(frame.src).pathname, elements: frame.contentDocument?.getElementsByTagName('*').length ?? null,
              manifestVersion: frame.contentDocument?.querySelector('meta[name="rainy-icesky-version"]')?.content ?? null,
              visible: rect.width > 0 && rect.height > 0, ready: frame.contentWindow?.app?._isVue === true,
              activeTool: frame.contentWindow?.app?.activeTab,
              inputChars: (frame.contentDocument?.querySelector('#transform-input')?.value
                ?? frame.contentDocument?.querySelector('#transform-input-single')?.value ?? '').length,
              transformAssetsReady: (frame.contentWindow?.app?.getToolView?.('transforms') ?? frame.contentWindow?.app)?.transformAssetsReady === true,
              transformCount: (frame.contentWindow?.app?.getToolView?.('transforms') ?? frame.contentWindow?.app)?.transforms?.length ?? 0,
              context: frame.contentWindow?.IceSkyRuntime?.context ?? null }
          })
          const mainElements = document.getElementsByTagName('*').length
          return { mainElements, frameElements: frames.reduce((total, frame) => total + (frame.elements ?? 0), 0),
            totalElements: mainElements + frames.reduce((total, frame) => total + (frame.elements ?? 0), 0),
            iframeCount: frames.length, visibleIframeCount: frames.filter(frame => frame.visible).length, frames }
        })
        assert.equal(footprint.visibleIframeCount, 1)
        assert(footprint.frames.every(frame => frame.ready && frame.elements > 0))
        assert(footprint.frames.every(frame => frame.activeTool === 'transforms' && frame.inputChars === 0))
        assert(footprint.frames.every(frame => frame.transformAssetsReady && frame.transformCount > 0))
        assert.equal(executionContexts.length, footprint.iframeCount + 1, 'main CDP target must observe the main document and every measured frame')
        assert(executionContexts.every(value => value.origin === harness.origin), 'measured document contexts must share the private local origin')
        const selections = []
        for (let selection = 0; selection < switchesPerSample; selection++) {
          const target = chats === 1 ? 1 : selection % 2 === 0 ? 1 : chats
          const id = `icesky-ui-fixture-${target}`
          const started = performance.now()
          await page.locator(`[data-row-key="session:${id}"]`).click()
          await readyWorkbench(page, id)
          await page.evaluate(async () => {
            const frame = Array.from(document.querySelectorAll('iframe')).find(frame => frame.getBoundingClientRect().width > 0)
            if (frame?.contentWindow?.IceSkyRuntime?.flush) await frame.contentWindow.IceSkyRuntime.flush()
          })
          selections.push({ target, elapsedMs: performance.now() - started })
        }
        await cdp.detach()
        report.observations.push({ chats, sample: index, selectionKind: chats === 1 ? 'same-chat-reselection' : 'cross-chat-switch',
          footprint, heap, selections })
        report.observations.at(-1).mainTargetDefaultContexts = executionContexts
        console.log(JSON.stringify({ label, chats, sample: index + 1, iframeCount: footprint.iframeCount,
          totalElements: footprint.totalElements, usedHeapBytes: heap.usedSize }))
      } finally { await context.close() }
    }
  }
  report.summary = Object.fromEntries([1, 5, 10].map(chats => {
    const group = report.observations.filter(value => value.chats === chats)
    return [chats, { iframeCount: summary(group.map(value => value.footprint.iframeCount)),
      mainElements: summary(group.map(value => value.footprint.mainElements)), frameElements: summary(group.map(value => value.footprint.frameElements)),
      totalElements: summary(group.map(value => value.footprint.totalElements)), heapUsedBytes: summary(group.map(value => value.heap.usedSize)),
      heapTotalBytes: summary(group.map(value => value.heap.totalSize)),
      selectionKind: chats === 1 ? 'same-chat-reselection' : 'cross-chat-switch',
      selectionMs: summary(group.flatMap(value => value.selections.map(selection => selection.elapsedMs))) }]
  }))
} catch (error) {
  report.failure = error.stack
  process.exitCode = 1
} finally {
  report.finishedAtUtc = new Date().toISOString()
  report.blockedExternalHosts = [...new Set(report.blockedExternalHosts)]
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify({ output, summary: report.summary, failure: report.failure?.split('\n')[0],
    errorCount: report.errors.length, consoleErrorCount: report.consoleErrors.length,
    consoleErrorExamples: report.consoleErrors.slice(0, 2).map(value => ({ ...value, message: value.message.slice(0, 260) })),
    blockedExternalHosts: report.blockedExternalHosts }))
  await harness.stop()
}
