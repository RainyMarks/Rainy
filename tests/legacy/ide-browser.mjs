/** Real Rainy workspace and Monaco interaction through a private WSL profile. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { randomUUID } from 'node:crypto'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro, user] = process.argv.slice(2)
assert(runtime && distro && user, 'Pass the prepared runtime, isolated distribution and test user')
const output = resolve('apps/rainy-desktop/validation/ide-runtime/browser')
await mkdir(output, { recursive: true })
const home = `/var/tmp/rainy-ide-browser-${randomUUID()}`
const harness = await openWorkbenchHarness({ runtime, distro, user, home, fixture: true })
const checks = []
const errors = []
const page = await harness.context.newPage()
page.setDefaultTimeout(30000)
page.on('pageerror', error => errors.push(error.message))
page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
async function api(body) {
  const response = await harness.context.request.post(`${harness.origin}/rainy/ide`, { data: body })
  const result = await response.json()
  assert.equal(response.status(), 200, JSON.stringify(result))
  assert.equal(result.ok, true, JSON.stringify(result))
  return result.value
}
async function until(read, description) {
  const deadline = Date.now() + 30000
  while (Date.now() < deadline) { const value = await read(); if (value) return value; await new Promise(accept => setTimeout(accept, 75)) }
  throw new Error(description)
}
async function openFile(name) {
  await page.getByRole('treeitem').filter({ hasText: name }).click()
  await page.locator('[data-rainy-monaco] .monaco-editor:visible').first().waitFor({ timeout: 60000 })
}

try {
  const workspace = (await api({ op: 'workspaces.list' }))[0]
  assert(workspace)
  const workspaceId = workspace.workspaceId
  const initial = Array.from({ length: 599 }, (_, index) => `# 行 ${index + 1}`).join('\r\n') + '\r\nsentinel = 600\r\n'
  await api({ op: 'files.create', workspaceId, path: 'long.py', content: '\uFEFF' + initial })
  await api({ op: 'files.create', workspaceId, path: 'broken.py', content: 'value: int = 1\n' })
  await page.goto(harness.origin, { waitUntil: 'load' })
  const welcome = page.locator('[class*="onboardingOverlay"]')
  if (await welcome.count()) await welcome.getByRole('button').click()
  await page.locator('[data-rainy-ide]').waitFor({ timeout: 60000 })
  await page.getByRole('combobox', { name: /^(工作区|Workspace)$/ }).selectOption(workspaceId)
  await page.getByRole('tree', { name: /^(文件|Files)$/ }).waitFor()
  assert.equal(await page.locator('[data-rainy-rail]').count(), 0)
  await page.screenshot({ path: resolve(output, 'initial.png') })
  checks.push({ name: 'actual Rainy profile mounts the three-pane IDE without the old activity rail', passed: true })
  await openFile('long.py')
  await page.locator('[data-rainy-monaco] .view-lines:visible').first().click({ position: { x: 65, y: 12 } })
  await page.keyboard.press('Control+End')
  await page.getByText('sentinel = 600', { exact: false }).last().waitFor()
  checks.push({ name: 'Monaco opens and navigates the complete 600-line document', passed: true })
  await page.keyboard.press('Control+a')
  await page.keyboard.insertText('value = 41\nprint(value + 1)\n')
  const saveResponse = page.waitForResponse(response => response.url().endsWith('/rainy/ide')
    && response.request().method() === 'POST' && response.request().postDataJSON()?.op === 'files.save')
  await page.keyboard.press('Control+s')
  assert.equal((await (await saveResponse).json()).ok, true)
  const saved = await api({ op: 'files.read', workspaceId, path: 'long.py' })
  assert.equal(saved.content, 'value = 41\r\nprint(value + 1)\r\n')
  assert.equal(saved.bom, true)
  await page.keyboard.press('Control+z')
  await until(async () => (await api({ op: 'state.read', workspaceId })).data.buffers.some(buffer => buffer.path === 'long.py' && buffer.content !== saved.content), 'Undo was not retained in the recovery snapshot')
  assert.equal((await api({ op: 'files.read', workspaceId, path: 'long.py' })).content, saved.content)
  await page.keyboard.press('Control+y')
  await until(async () => !(await api({ op: 'state.read', workspaceId })).data.buffers.some(buffer => buffer.path === 'long.py'), 'Redo did not restore the saved editor content')
  checks.push({ name: 'keyboard edit/save/undo/redo preserves BOM, CRLF and durable unsaved recovery', passed: true })
  await openFile('broken.py')
  await page.locator('[data-rainy-monaco] .view-lines:visible').first().click({ position: { x: 65, y: 12 } })
  await page.keyboard.press('Control+a')
  await page.keyboard.insertText('value: int = "wrong"\n')
  await until(async () => (await page.locator('[data-rainy-ide]').innerText()).includes('python: ready') || await page.locator('.squiggly-error').count() > 0,
    'Unsaved-document language diagnostics did not reach Monaco')
  await page.screenshot({ path: resolve(output, 'editor-diagnostics.png') })
  assert.equal((await api({ op: 'files.read', workspaceId, path: 'broken.py' })).content, 'value: int = 1\n')
  checks.push({ name: 'real language-server diagnostics reach the Monaco editor', passed: true })
  assert.deepEqual(harness.blocked, [])
  assert.deepEqual(errors, [])
  await writeFile(resolve(output, 'acceptance.json'), JSON.stringify({ passed: true, runtime, distro, home, checks, errors }, null, 2) + '\n')
  console.log(JSON.stringify({ passed: true, checks }))
} catch (error) {
  await page.screenshot({ path: resolve(output, 'failure.png') }).catch(() => {})
  await writeFile(resolve(output, 'failure.txt'), `${String(error)}\n${errors.join('\n')}\n${await page.locator('body').innerText().catch(() => '')}`)
  await writeFile(resolve(output, 'acceptance.json'), JSON.stringify({ passed: false, runtime, distro, home, checks, errors, error: String(error) }, null, 2) + '\n')
  throw error
} finally { await harness.stop() }
