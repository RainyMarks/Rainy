/** Real-profile checks for compact workspace chrome and viewport-bounded dialogs. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { randomUUID } from 'node:crypto'
import { resolve } from 'node:path'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro = 'Ubuntu', user = 'rog', mode = 'after'] = process.argv.slice(2)
assert(runtime && ['before', 'after'].includes(mode))
const output = resolve('apps/rainy-desktop/validation/ui-simplification', mode)
await mkdir(output, { recursive: true })
const home = `/var/tmp/rainy-layout-${randomUUID()}`
const project = `${home}/project`
await promisify(execFile)('wsl.exe', ['-d', distro, '-u', user, '--exec', 'python3', '-c',
  'import pathlib,sys; pathlib.Path(sys.argv[1]).mkdir(parents=True)', project], { windowsHide: true })
const harness = await openWorkbenchHarness({ runtime, distro, user, home })
const page = await harness.context.newPage()
const errors = []
const checks = []
const geometry = []
const chrome = []
page.setDefaultTimeout(30000)
page.on('pageerror', error => errors.push(error.message))
page.on('console', message => { if (message.type() === 'error') errors.push(message.text()) })
const topbar = page.locator('[data-rainy-topbar]')
async function api(body) {
  const response = await harness.context.request.post(`${harness.origin}/rainy/ide`, { data: body })
  const value = await response.json()
  assert.equal(response.status(), 200, JSON.stringify(value))
  assert(value.ok)
  return value.value
}
async function until(read, message) {
  const deadline = Date.now() + 30000
  while (Date.now() < deadline) {
    if (await read()) return
    await new Promise(accept => setTimeout(accept, 80))
  }
  throw new Error(message)
}
async function capture(name) { await page.screenshot({ path: resolve(output, `${name}.png`), animations: 'disabled' }) }
async function theme(name) {
  await topbar.getByRole('button', { name: /^(设置|Settings)$/ }).click()
  await page.locator('#rainy-preferences-tab').click()
  await page.getByRole('button', { name: name === 'dark' ? /^(深色|Dark)$/ : /^(浅色|Light)$/ }).click()
  await page.keyboard.press('Escape')
}
async function openConfiguration() {
  const file = topbar.getByRole('button', { name: /^(文件|File)$/ })
  if (await file.count()) await file.click()
  else {
    await topbar.getByRole('button', { name: /^(应用菜单|Application menu)$/ }).click()
    await page.getByRole('menuitem', { name: /^(文件|File)$/ }).hover()
  }
  await page.getByRole('menuitem', { name: /^(运行配置|Run configurations)$/ }).click()
  return page.getByRole('dialog', { name: /^(运行配置|Run configurations)$/ })
}
async function measure(dialog, name) {
  const result = await dialog.evaluate((element) => {
    const card = element.getBoundingClientRect()
    const fields = [...element.querySelectorAll('input,select,button')].filter(field => field.getClientRects().length > 0)
      .map(field => {
        const rect = field.getBoundingClientRect()
        return { tag: field.tagName, left: rect.left, right: rect.right, width: rect.width }
      })
    return { viewport: { width: innerWidth, height: innerHeight }, card: { left: card.left, right: card.right, top: card.top, bottom: card.bottom },
      scrollWidth: element.scrollWidth, clientWidth: element.clientWidth, fields,
      outside: fields.filter(field => field.left < card.left - 1 || field.right > card.right + 1) }
  })
  geometry.push({ name, ...result })
  if (mode === 'after') {
    assert(result.card.left >= 12 && result.card.right <= result.viewport.width - 12, name)
    assert(result.card.top >= 12 && result.card.bottom <= result.viewport.height - 12, name)
    assert(result.scrollWidth <= result.clientWidth + 1, name)
    assert.equal(result.outside.length, 0, JSON.stringify({ name, outside: result.outside }))
  }
  return result
}

let passed = false
try {
  const workspace = await api({ op: 'workspaces.open', path: project })
  const workspaceId = workspace.workspaceId
  await api({ op: 'files.create', workspaceId, path: 'main.py', content: 'print("layout")\n' })
  await api({ op: 'files.create', workspaceId, path: 'README.md', content: '# Workspace\n' })
  await api({ op: 'files.create', workspaceId, path: 'pyproject.toml', content: '[project]\nname = "workspace"\n' })
  await api({ op: 'files.mkdir', workspaceId, path: 'src' })
  await api({ op: 'files.mkdir', workspaceId, path: 'tests' })
  await api({ op: 'files.create', workspaceId, path: 'long-project-filename-这是一段需要在文件树内截断显示的完整文件名.py', content: '' })
  for (let index = 0; index < 14; index++) await api({ op: 'files.create', workspaceId,
    path: `tests/workspace-file-${String(index).padStart(2, '0')}-这是一段需要在文件树和搜索框内完整保留但截断显示的名称.py`, content: '' })
  await page.goto(harness.origin)
  const onboarding = page.locator('[class*="onboardingOverlay"]')
  if (await onboarding.count()) await onboarding.getByRole('button').click()
  await topbar.waitFor({ timeout: 60000 })
  await theme('dark')
  await page.getByRole('combobox', { name: /^(工作区|Workspace)$/ }).selectOption(workspaceId)
  await page.getByRole('treeitem').filter({ hasText: 'main.py' }).waitFor()
  const initial = await api({ op: 'state.read', workspaceId })
  if (mode === 'after') {
    assert.equal(initial.data.layout.agentVisible, false)
    assert.equal(initial.data.layout.bottomVisible, false)
    assert.equal(await page.getByRole('tablist', { name: /^(文件|Files)$/ }).count(), 0)
    assert.equal(await topbar.getByRole('button', { name: /^(运行|Run)$/ }).count(), 0)
    const tree = await page.getByRole('tree', { name: /^(文件|Files)$/ }).evaluate(element => ({ width: element.clientWidth, scroll: element.scrollWidth }))
    assert(tree.scroll <= tree.width + 1, JSON.stringify(tree))
    checks.push('Fresh workspace has only files and main content; long filenames stay inside the file pane')
  }
  await capture('workspace-dark-1380')
  await topbar.getByRole('button', { name: /^(搜索工作区文件|Search workspace files)$/ }).click()
  const quick = page.getByRole('dialog', { name: /^(搜索工作区文件|Search workspace files)$/ })
  await quick.getByRole('option').first().waitFor()
  await measure(quick, 'quick-open-1380')
  await capture('quick-open-1380')
  await page.keyboard.press('Escape')
  await page.getByRole('treeitem').filter({ hasText: 'main.py' }).click()
  await page.getByRole('tab', { name: 'main.py', exact: true }).waitFor()
  const configuration = await openConfiguration()
  await measure(configuration, 'run-config-1380')
  await capture('run-config-1380')
  if (mode === 'before') {
    checks.push('Recorded original dialog geometry and screenshots before rebuilding')
    passed = true
  } else {
    const advanced = configuration.locator('details')
    assert.equal(await advanced.getAttribute('open'), null)
    await advanced.locator('summary').click()
    await configuration.getByRole('textbox', { name: /^(配置名称|Configuration name)$/ }).fill('Layout profile')
    await configuration.getByRole('textbox', { name: /^(参数（JSON 数组）|Arguments \(JSON array\))$/ }).fill('["--sample","two words"]')
    await configuration.getByRole('textbox', { name: /^(环境变量（JSON 对象）|Environment \(JSON object\))$/ }).fill('{"LAYOUT":"kept"}')
    await configuration.getByRole('button', { name: /^(保存|Save)$/ }).click()
    await until(async () => (await api({ op: 'state.read', workspaceId })).data.execution?.profiles.some(value => value.name === 'Layout profile'), 'Run configuration persistence')
    const saved = (await api({ op: 'state.read', workspaceId })).data.execution.profiles.find(value => value.name === 'Layout profile')
    assert.deepEqual(saved.arguments, ['--sample', 'two words'])
    assert.deepEqual(saved.environment, { LAYOUT: 'kept' })
    checks.push('Collapsed advanced options retain arguments and environment through the ordinary save path')

    await topbar.getByRole('button', { name: /^(显示或隐藏 AI 助手|Show or hide AI assistant)$/ }).click()
    const agent = page.getByRole('complementary', { name: /^(AI 助手|AI assistant)$/ })
    const draft = agent.locator('[contenteditable="true"]').first()
    await draft.fill('This draft must survive panel changes')
    await topbar.getByRole('button', { name: /^(CTF 工具|CTF tools)$/ }).click()
    await page.getByRole('tab', { name: /^(CTF 工具|CTF tools)$/ }).waitFor()
    assert.equal(await page.locator('[data-rainy-ctf-workbench] header').count(), 1)
    await capture('tools-and-ai-1380')
    await page.getByRole('button', { name: /^(收起 CTF 工具|Hide CTF tools)$/ }).click()
    assert.equal(await page.getByRole('tab', { name: /^(CTF 工具|CTF tools)$/ }).count(), 0)
    await topbar.getByRole('button', { name: /^(视图|View)$/ }).click()
    await page.getByRole('menuitem', { name: /^(专注编辑|Focus editor)$/ }).click()
    assert.equal(await agent.count(), 0)
    await topbar.getByRole('button', { name: /^(显示或隐藏 AI 助手|Show or hide AI assistant)$/ }).click()
    assert.equal(await draft.innerText(), 'This draft must survive panel changes')
    checks.push('Focus editor and tool-tab close retain the same AI draft')
    await capture('editor-and-ai-1380')

    await theme('light')
    await topbar.getByRole('button', { name: /^(显示或隐藏 AI 助手|Show or hide AI assistant)$/ }).click()
    await page.setViewportSize({ width: 950, height: 650 })
    await capture('editor-light-950')
    for (const viewport of [{ width: 950, height: 650 }, { width: 640, height: 480 }]) {
      await page.setViewportSize(viewport)
      await page.evaluate(() => { document.body.style.setProperty('--dsh-content-font-size', '20px') })
      const bar = await topbar.evaluate(element => ({ viewport: innerWidth,
        fontFamily: getComputedStyle(element).fontFamily,
        actions: [...element.querySelectorAll('button,select')].filter(item => item.getClientRects().length > 0)
          .map(item => ({ label: item.getAttribute('aria-label') ?? item.textContent, left: item.getBoundingClientRect().left, right: item.getBoundingClientRect().right })) }))
      chrome.push(bar)
      assert(bar.actions.every(item => item.left >= 0 && item.right <= viewport.width - 145), 'Toolbar must clear native window controls')
      const workspaceControl = bar.actions.find(item => /Workspace|工作区/.test(item.label ?? ''))
      assert(workspaceControl && workspaceControl.right - workspaceControl.left >= 70, 'Workspace selector stays usable')
      const runDialog = await openConfiguration()
      await measure(runDialog, `run-config-${viewport.width}-20px`)
      await runDialog.locator('summary').click()
      await runDialog.getByRole('textbox', { name: /解释器或编译器|Interpreter or compiler/ }).scrollIntoViewIfNeeded()
      await measure(runDialog, `advanced-${viewport.width}-20px`)
      const save = await runDialog.getByRole('button', { name: /^(保存|Save)$/ }).boundingBox()
      assert(save && save.y >= 0 && save.y + save.height <= viewport.height)
      await capture(`advanced-${viewport.width}-20px`)
      await page.keyboard.press('Escape')
      await topbar.getByRole('button', { name: /^(搜索工作区文件|Search workspace files)$/ }).click()
      const search = page.getByRole('dialog', { name: /^(搜索工作区文件|Search workspace files)$/ })
      await search.getByRole('option').first().waitFor()
      await measure(search, `quick-open-${viewport.width}-20px`)
      await search.getByRole('combobox').fill('main.py')
      await until(async () => await search.getByRole('option').count() === 1, 'Filtered filename')
      await page.keyboard.press('Enter')
      await until(async () => await search.count() === 0, 'Quick open closes after keyboard selection')
    }
    checks.push('Light theme, 950px and 640px logical viewports, and 20px interface text keep dialogs and their Save action in view')
    assert.equal(errors.length, 0, errors.join('\n'))
    assert.equal(harness.blocked.length, 0)
    passed = true
  }
} catch (error) {
  await capture('failure').catch(() => {})
  await writeFile(resolve(output, 'failure.txt'), error.stack ?? String(error))
  throw error
} finally {
  await writeFile(resolve(output, 'acceptance.json'), JSON.stringify({ passed, mode, runtime, distro, user, home, project,
    checks, geometry, chrome, errors, outside: harness.blocked, scope: 'Isolated real Rainy profile and headless browser; no native window interaction' }, null, 2) + '\n')
  await harness.stop()
}
console.log(JSON.stringify({ passed, mode, checks: checks.length, measurements: geometry.length, output }))
