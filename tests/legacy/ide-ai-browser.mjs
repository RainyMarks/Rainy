/** Real Assistant code actions, ordinary selected-code messages, and IDE appearance controls. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { randomUUID } from 'node:crypto'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro, user] = process.argv.slice(2)
assert(runtime && distro && user)
const output = resolve('apps/rainy-desktop/validation/ide-runtime/ai-ui')
await mkdir(output, { recursive: true })
const home = `/var/tmp/rainy-ide-ai-${randomUUID()}`
const harness = await openWorkbenchHarness({ runtime, distro, user, home, fixture: true, assistantCode: true })
const run = promisify(execFile)
const page = await harness.context.newPage()
const errors = []
const checks = []
page.on('pageerror', error => errors.push(error.message))
page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
page.setDefaultTimeout(30000)
const topbar = page.locator('[data-rainy-topbar]')
const appearanceOnly = process.argv.includes('--appearance-only')
async function api(body) {
  const response = await harness.context.request.post(`${harness.origin}/rainy/ide`, { data: body })
  const value = await response.json()
  assert.equal(response.status(), 200, JSON.stringify(value))
  assert.equal(value.ok, true, JSON.stringify(value))
  return value.value
}
async function until(read, message) {
  const deadline = Date.now() + 30000
  while (Date.now() < deadline) { const value = await read(); if (value) return value; await new Promise(accept => setTimeout(accept, 75)) }
  throw new Error(message)
}
async function preferences() {
  await topbar.getByRole('button', { name: /^(设置|Settings)$/ }).click()
  await page.locator('#rainy-preferences-tab').click()
  await page.getByRole('button', { name: /^(深色|Dark)$/ }).waitFor()
}

try {
  const workspace = (await api({ op: 'workspaces.list' }))[0]
  const workspaceId = workspace.workspaceId
  const original = 'before = 1\nprint(before)\n'
  await api({ op: 'files.create', workspaceId, path: 'target.py', content: original })
  await page.goto(harness.origin)
  const welcome = page.locator('[class*="onboardingOverlay"]')
  if (await welcome.count()) await welcome.getByRole('button').click()
  await topbar.waitFor({ timeout: 60000 })
  await page.getByRole('combobox', { name: /^(工作区|Workspace)$/ }).selectOption(workspaceId)
  await page.getByRole('treeitem').filter({ hasText: 'target.py' }).click()
  await page.locator('[data-rainy-monaco] .view-lines:visible').first().waitFor({ timeout: 60000 })
  const agent = page.getByRole('complementary', { name: /^(AI 助手|AI assistant)$/ })
  if (!appearanceOnly) {
  await agent.getByRole('button', { name: /^(对话历史|Chat history)$/ }).click()
  const sessionId = harness.ready.fixtureSessionIds[0]
  const sessionRow = page.locator(`[data-row-key="session:${sessionId}"]`)
  if (await sessionRow.count() === 0) {
    const expand = page.getByRole('button', { name: /展开其余.*会话|Show.*more sessions|Expand.*sessions/ })
    if (await expand.count()) await expand.first().click()
  }
  await sessionRow.click()
  const openCode = agent.getByRole('button', { name: /^(在编辑器中打开|Open in editor)$/ })
  await openCode.waitFor()
  await page.getByRole('tab', { name: /^(文件|Files)$/ }).first().click()
  await openCode.click()
  await page.getByRole('tab', { name: /AI (代码片段|snippet)/ }).waitFor()
  assert.equal((await api({ op: 'files.read', workspaceId, path: 'target.py' })).content, original)
  checks.push({ name: 'Assistant code opens a separate readonly editor tab and leaves the project file intact', passed: true })
  await page.getByRole('tab', { name: 'target.py', exact: true }).click()
  await agent.getByRole('button', { name: /^(与当前文件比较|Compare with file)$/ }).click()
  const apply = page.getByRole('button', { name: /^(应用到文件缓冲区|Apply to file buffer)$/ })
  await apply.waitFor()
  assert.equal((await api({ op: 'files.read', workspaceId, path: 'target.py' })).content, original)
  await apply.click()
  await until(async () => (await api({ op: 'state.read', workspaceId })).data.buffers.some(buffer => buffer.path === 'target.py' && buffer.content.includes('value = 41')), 'Reviewed code was not retained as an unsaved project buffer')
  assert.equal((await api({ op: 'files.read', workspaceId, path: 'target.py' })).content, original)
  checks.push({ name: 'Reviewed Assistant diff changes only the target buffer until an explicit save', passed: true })
  await page.locator('[data-rainy-monaco] .view-lines:visible').first().click({ position: { x: 80, y: 12 } })
  await page.keyboard.press('Control+a')
  await page.getByRole('button', { name: /^(将选中代码发送到 AI|Send selection to AI)$/ }).click()
  await until(async () => (await agent.innerText()).includes('target.py'), 'Selected code did not appear as an ordinary user message')
  const logged = await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'python3', '-c', String.raw`
import json,sys
from pathlib import Path
root=Path(sys.argv[1])/'sessions'
matches=[]
for path in root.rglob('*.jsonl'):
    for line in path.read_text().splitlines():
        if 'target.py' in line and 'value = 41' in line:
            matches.append({'file':path.name,'event':json.loads(line)})
print(json.dumps(matches))
`, home], { windowsHide: true })
  const messages = JSON.parse(logged.stdout)
  assert(messages.length > 0, 'No durable Session event contains the selection')
  assert(messages.some(row => JSON.stringify(row.event).includes('user')), 'Selection event is not a normal user message')
  checks.push({ name: 'Selected path, range and source are retained in ordinary durable user-message events', passed: true, matchingEvents: messages.length })
  }
  const beforeFont = await page.locator('[data-rainy-monaco] .view-lines:visible').first().evaluate(element => getComputedStyle(element).fontSize)
  await preferences()
  await page.getByRole('button', { name: /^(深色|Dark)$/ }).click()
  await until(() => page.locator('body').evaluate(element => element.hasAttribute('data-ds-dark-theme')), 'Dark theme did not apply')
  await page.getByRole('button', { name: /^(增大代码字号|Increase code font size)$/ }).click()
  await page.keyboard.press('Escape')
  await until(async () => parseFloat(await page.locator('[data-rainy-monaco] .view-lines:visible').first().evaluate(element => getComputedStyle(element).fontSize)) > parseFloat(beforeFont), 'Code font preference did not reach Monaco')
  checks.push({ name: 'Unified settings retain appearance and code-font preferences', passed: true })
  const separator = page.getByRole('separator', { name: /调整 AI.*宽度|Resize AI/ })
  const initialWidth = await agent.evaluate(element => element.getBoundingClientRect().width)
  await separator.focus()
  await page.keyboard.press('ArrowLeft')
  await until(async () => (await agent.evaluate(element => element.getBoundingClientRect().width)) > initialWidth, 'Keyboard panel resize failed')
  await page.keyboard.press('ArrowRight')
  const handle = await separator.boundingBox()
  assert(handle)
  await page.mouse.move(handle.x + handle.width / 2, handle.y + 150)
  await page.mouse.down()
  await page.mouse.move(handle.x - 32, handle.y + 150, { steps: 4 })
  await page.mouse.up()
  await until(async () => (await agent.evaluate(element => element.getBoundingClientRect().width)) > initialWidth, 'Pointer panel resize failed')
  checks.push({ name: 'Panel separators respond to keyboard and pointer resizing', passed: true })
  for (const theme of ['dark', 'light']) {
    if (theme === 'light') {
      await preferences()
      await page.getByRole('button', { name: /^(浅色|Light)$/ }).click()
      await page.keyboard.press('Escape')
    }
    for (const width of [1380, 950]) {
      await page.setViewportSize({ width, height: 920 })
      if (width === 950) await until(() => page.locator('aside[aria-label="工作区"], aside[aria-label="Workspace"]').isHidden(), 'A narrow window did not collapse the file pane')
      await until(() => page.locator('[data-rainy-monaco] .view-line:visible').first().evaluate(element => {
        const bounds = element.getBoundingClientRect()
        const target = document.elementFromPoint(bounds.x + 40, bounds.y + bounds.height / 2)
        return target !== null && element.contains(target)
      }), 'The source line was obscured after resizing')
      await page.screenshot({ path: resolve(output, `ide-${theme}-${width}.png`) })
      assert((await agent.boundingBox()).width >= 300)
      assert(await page.locator('[data-rainy-monaco] .monaco-editor:visible').first().isVisible())
    }
  }
  checks.push({ name: 'Dark and light themes retain visible source at 1380px and 950px; narrow windows collapse the file pane', passed: true })
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
