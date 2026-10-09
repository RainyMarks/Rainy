/** Real Monaco rename across an open source and an unopened importing file. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { randomUUID } from 'node:crypto'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro, user, label = 'current'] = process.argv.slice(2)
assert(runtime && distro && user, 'Pass the prepared runtime, isolated distribution and test user')
const output = resolve('validation/ide-runtime/rename', label)
await mkdir(output, { recursive: true })
const home = `/var/tmp/rainy-ide-rename-${randomUUID()}`
const harness = await openWorkbenchHarness({ runtime, distro, user, home, fixture: true })
const page = await harness.context.newPage()
page.setDefaultTimeout(30000)
const errors = []
const frames = []
const checks = []
const observed = {}
page.on('pageerror', error => errors.push(error.message))
page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
page.on('websocket', socket => {
  if (!socket.url().includes('/rainy/ide/lsp')) return
  for (const event of ['framesent', 'framereceived']) socket.on(event, frame => {
    try { frames.push({ event, language: new URL(socket.url()).searchParams.get('language'), message: JSON.parse(String(frame.payload)) }) }
    catch (error) { errors.push(`Invalid LSP frame: ${String(error)}`) }
  })
})
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
async function report(passed, error) {
  await writeFile(resolve(output, 'acceptance.json'), JSON.stringify({ passed, runtime, distro, home, checks, observed, errors, error: error === undefined ? undefined : String(error) }, null, 2) + '\n')
  await writeFile(resolve(output, 'lsp.json'), JSON.stringify(frames, null, 2) + '\n')
}
try {
  const workspace = (await api({ op: 'workspaces.list' }))[0]
  assert(workspace)
  const workspaceId = workspace.workspaceId
  for (const [path, content] of Object.entries({
    'tsconfig.json': '{"compilerOptions":{"strict":true,"target":"ES2022","module":"ESNext","moduleResolution":"Bundler"},"include":["*.ts"]}\n',
    'helper.ts': 'export function answer(): number { return 42 }\n',
    'main.ts': 'import { answer } from "./helper"\nconsole.log(answer())\n',
  })) await api({ op: 'files.create', workspaceId, path, content })
  await page.goto(harness.origin, { waitUntil: 'load' })
  const welcome = page.locator('[class*="onboardingOverlay"]')
  if (await welcome.count()) await welcome.getByRole('button').click()
  await page.locator('[data-rainy-ide]').waitFor({ timeout: 60000 })
  await page.getByRole('combobox', { name: /^(工作区|Workspace)$/ }).selectOption(workspaceId)
  await page.getByRole('treeitem').filter({ hasText: 'helper.ts' }).click()
  await page.locator('[data-rainy-monaco] .view-lines:visible').first().waitFor({ timeout: 60000 })
  await until(async () => frames.some(frame => frame.event === 'framereceived' && frame.message.method === 'textDocument/publishDiagnostics'), 'TypeScript language client did not publish its initial diagnostics', 60000)
  observed.before = await api({ op: 'state.read', workspaceId })
  assert(!observed.before.data.tabs.some(tab => tab.path === 'main.ts'))
  assert(!observed.before.data.buffers.some(buffer => buffer.path === 'main.ts'))
  checks.push({ name: 'main.ts remains unopened before the real editor rename', passed: true })
  await page.locator('[data-rainy-monaco] .view-lines:visible').first().click({ position: { x: 120, y: 12 } })
  await page.keyboard.press('Control+Home')
  for (let index = 0; index < 18; index++) await page.keyboard.press('ArrowRight')
  await page.keyboard.press('F2')
  const renameInput = page.locator('.rename-box input')
  await renameInput.waitFor()
  await renameInput.fill('renamedAnswer')
  await renameInput.press('Enter')
  await until(async () => (await api({ op: 'state.read', workspaceId })).data.buffers.some(buffer => buffer.path === 'helper.ts' && buffer.content.includes('renamedAnswer')), 'The renamed open source did not enter recovery')
  observed.afterRename = await api({ op: 'state.read', workspaceId })
  observed.tabsAfterRename = await page.getByRole('tablist', { name: /^(文件|Files)$/ }).innerText()
  await page.screenshot({ path: resolve(output, 'renamed.png') })
  const helperRecovery = observed.afterRename.data.buffers.find(buffer => buffer.path === 'helper.ts')
  const mainRecovery = observed.afterRename.data.buffers.find(buffer => buffer.path === 'main.ts')
  assert.equal(helperRecovery?.content, 'export function renamedAnswer(): number { return 42 }\n')
  assert.equal(mainRecovery?.content, 'import { renamedAnswer } from "./helper"\nconsole.log(renamedAnswer())\n', 'The unopened importing file must enter dirty recovery')
  checks.push({ name: 'both rename edits enter durable unsaved recovery', passed: true })
  assert.equal((await api({ op: 'files.read', workspaceId, path: 'main.ts' })).content, 'import { answer } from "./helper"\nconsole.log(answer())\n')
  await page.getByRole('button', { name: /^(文件|File)$/ }).click()
  await page.getByRole('menuitem', { name: /^(全部保存|Save all)$/ }).click()
  await until(async () => (await api({ op: 'state.read', workspaceId })).data.buffers.length === 0, 'Save all did not clear the dirty recovery buffers')
  observed.afterSave = await api({ op: 'state.read', workspaceId })
  observed.savedHelper = await api({ op: 'files.read', workspaceId, path: 'helper.ts' })
  observed.savedMain = await api({ op: 'files.read', workspaceId, path: 'main.ts' })
  assert.equal(observed.savedHelper.content, helperRecovery.content)
  assert.equal(observed.savedMain.content, mainRecovery.content)
  checks.push({ name: 'Save all writes both files and clears their recovery buffers', passed: true })
  assert.deepEqual(harness.blocked, [])
  assert.deepEqual(errors, [])
  await report(true)
  console.log(JSON.stringify({ passed: true, output, checks }))
} catch (error) {
  await page.screenshot({ path: resolve(output, 'failure.png') }).catch(() => {})
  await writeFile(resolve(output, 'failure.txt'), `${String(error)}\n${errors.join('\n')}\n${await page.locator('body').innerText().catch(() => '')}`)
  await report(false, error)
  throw error
} finally { await harness.stop() }
