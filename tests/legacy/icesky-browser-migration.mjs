/** Legacy browser draft ownership through a real packaged Host and failed save acknowledgement. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const runtime = process.argv[2]
if (!runtime) throw new Error('Pass an installed or staged Linux runtime directory.')
const output = resolve('apps/rainy-desktop/validation/icesky-browser-migration')
await mkdir(output, { recursive: true })
const h = await openWorkbenchHarness({ runtime })
const page = await h.context.newPage()
const report = { runtime, checks: [], errors: [] }
page.on('pageerror', error => report.errors.push(error.message))
try {
  await page.addInitScript(() => {
    if (sessionStorage.getItem('migration-fixture-seeded')) return
    localStorage.setItem('pc-draft-v2', JSON.stringify({ input: 'Legacy ordinary draft 示例🙂', strategy: 'clarify' }))
    sessionStorage.setItem('migration-fixture-seeded', '1')
  })
  const unavailable = route => route.request().method() === 'PUT'
    ? route.fulfill({ status: 503, contentType: 'application/json', body: '{"error":"Fixture unavailable"}' })
    : route.continue()
  await page.route('**/rainy/icesky/state?scope=standalone', unavailable)
  await page.goto(`${h.origin}/rainy/icesky/index.html?embed=rainy`)
  await page.waitForFunction(() => typeof window.app?.openTool === 'function' && window.app.toolLoading === false)
  assert.equal(await page.evaluate(async () => { try { await window.IceSkyRuntime.flush(); return true } catch { return false } }), false)
  assert(await page.evaluate(() => localStorage.getItem('pc-draft-v2')))
  report.checks.push('failed Host save retains the original browser draft')
  await page.unroute('**/rainy/icesky/state?scope=standalone', unavailable)
  await page.evaluate(async () => { await window.IceSkyRuntime.flush() })
  assert.equal(await page.evaluate(() => localStorage.getItem('pc-draft-v2')), null)
  const saved = await h.context.request.get(`${h.origin}/rainy/icesky/state?scope=standalone`).then(response => response.json())
  assert.equal(saved.data.tools.promptcraft.fields.pcInput, 'Legacy ordinary draft 示例🙂')
  assert.equal(saved.data.legacyMigrated, true)
  report.checks.push('confirmed Host save owns the imported text before deleting the browser key')
  await page.evaluate(() => localStorage.setItem('pc-draft-v2', JSON.stringify({ input: 'Later legacy key must not overwrite' })))
  await page.reload()
  await page.waitForFunction(() => typeof window.app?.openTool === 'function' && window.app.toolLoading === false)
  assert.equal(await page.evaluate(() => window.app.getToolState('promptcraft').pcInput), 'Legacy ordinary draft 示例🙂')
  assert(await page.evaluate(() => localStorage.getItem('pc-draft-v2')))
  report.checks.push('a later legacy key cannot overwrite the already migrated standalone draft')
  assert.deepEqual(report.errors, [])
} catch (error) {
  report.failure = error.stack
  process.exitCode = 1
} finally {
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify(report))
  await h.stop()
}
