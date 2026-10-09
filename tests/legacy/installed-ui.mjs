/** Exercise the signed installed carrier through its own page with isolated user data. */
import assert from 'node:assert/strict'
import { mkdir, readFile, realpath, writeFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { createHash } from 'node:crypto'
import { createRequire } from 'node:module'
const repository = resolve(import.meta.dirname, '../../..')
const [dataArgument, executableArgument, phase = 'initial', kindArgument] = process.argv.slice(2)
assert(dataArgument && executableArgument, 'Pass the isolated data directory and installed EXE')
assert(['initial', 'restart'].includes(phase), 'Phase must be initial or restart')
const data = resolve(dataArgument)
const executable = resolve(executableArgument)
const activationPath = join(data, 'license.dat')
async function activationBytes() {
  try { return await readFile(activationPath) }
  catch (error) { if (error.code === 'ENOENT') return null; throw error }
}
const priorActivation = await activationBytes()
const packaged = executable.replaceAll('\\', '/').includes('/release/win-unpacked/')
const kind = kindArgument ?? (packaged ? 'packaged' : 'installed')
assert(['packaged', 'installed', 'relocated'].includes(kind), 'Evidence kind must describe the tested carrier location')
const version = JSON.parse(await readFile(join(repository, 'apps/rainy-desktop/package.json'), 'utf8')).version
const evidence = join(repository, `validation/source-available-${version}`, `${kind}-client`)
await mkdir(evidence, { recursive: true })
const [port, endpoint] = (await readFile(join(data, 'DevToolsActivePort'), 'utf8')).trim().split(/\r?\n/)
const { chromium } = createRequire(join(repository, 'apps/web/package.json'))('playwright')
const browser = await chromium.connectOverCDP(`ws://127.0.0.1:${port}${endpoint}`)
const report = { phase, kind: `${kind}-carrier-native-windows`, executable, data,
  checks: [], pageErrors: [], consoleErrors: [], screenshots: [],
  executableSha256: createHash('sha256').update(await readFile(executable)).digest('hex'),
  resourceManifestSha256: createHash('sha256').update(await readFile(join(dirname(executable), 'resources/release-manifest.signed.json'))).digest('hex') }
const until = async (read, description, milliseconds = 60000) => {
  const deadline = Date.now() + milliseconds
  while (Date.now() < deadline) {
    if (!browser.isConnected()) throw new Error(`Carrier disconnected: ${description}`)
    const value = await read(); if (value) return value; await new Promise(accept => setTimeout(accept, 100))
  }
  throw new Error(description)
}
const primary = join(data, '验收项目')
const originalText = 'value = 1\n'
const dirtyText = 'value = 41\nprint(value + 1)\n'
let page
try {
  page = await until(() => browser.contexts().flatMap(context => context.pages())
    .find(page => page.url().startsWith('http://127.0.0.1:')), 'Native Host page did not load', 180000)
  page.setDefaultTimeout(30000)
  page.on('pageerror', error => report.pageErrors.push(error.message))
  page.on('console', message => {
    if (message.type() === 'error') report.consoleErrors.push(message.text().slice(0, 3000))
  })
  await page.locator('[data-rainy-topbar]').waitFor()
  assert.equal(await page.evaluate(() => '__RAINY_LICENSE__' in globalThis), false)
  report.checks.push('the carrier opens its authenticated workbench without activation')
  assert.equal(await page.evaluate(() => globalThis.__RAINY_AGENT__?.version), version)
  report.checks.push(`the authenticated Host publishes the application manifest version ${version}`)
  const strata = await page.evaluate(() => globalThis.__RAINY_STRATA_NATIVE__.status())
  assert.equal(strata.runtime.available, true)
  assert.equal(strata.runtime.version, '0.1.39')
  assert.equal(strata.settings.modelPath, '')
  assert.equal(strata.model, null)
  assert(['unconfigured', 'external'].includes(strata.phase), `Unexpected automatic Strata activity: ${strata.phase}`)
  assert.notEqual(strata.server?.owned, true)
  report.strata = { phase: strata.phase, version: strata.runtime.version, available: strata.runtime.available,
    serverOwned: strata.server?.owned ?? null }
  report.checks.push('the real native bridge finds bundled Strata 0.1.39 without selecting weights or starting a model')
  await until(async () => (await page.title()) === 'RainyAgent', 'Installed window title did not become RainyAgent')
  assert.equal(await page.title(), 'RainyAgent')
  if (phase === 'initial' && kind !== 'packaged') {
    const catalog = await page.evaluate(() => globalThis.__RAINY_TOOLS__.listTools())
    assert.equal(catalog.tools.length, 38, 'The upgrade fixture must retain its 38-entry tool manifest')
    const archivedTool = catalog.tools.find(tool => tool.id === 'yakit')
    assert(archivedTool)
    assert(!archivedTool.missing.some(path => path.includes('resources/app.asar')), 'The existing third-party ASAR must remain an ordinary available file')
    report.checks.push('the real desktop bridge scans the retained 38-entry catalog and opaque third-party ASAR without loading that archive')
  }
  const api = async params => {
    const response = await page.context().request.post(new URL('/rainy/ide', page.url()).href, { data: params })
    const result = await response.json(); assert.equal(response.status(), 200); assert.equal(result.ok, true, JSON.stringify(result)); return result.value
  }
  const selectedProject = async () => {
    const workspaces = await api({ op: 'workspaces.list' })
    assert.equal(workspaces.length, 1)
    const workspace = workspaces[0]
    assert.equal(workspace.path, await realpath(primary))
    assert.equal(workspace.roots.length, 1)
    const root = workspace.roots.find(root => root.primary)
    assert(root, 'The opened project must expose its primary root')
    assert.equal(root.path, workspace.path)
    const selection = await api({ op: 'state.selection.read' })
    assert.equal(selection.workspaceId, workspace.workspaceId)
    return { workspaceId: workspace.workspaceId, rootId: root.rootId }
  }
  const matchingDirtyBuffer = (state, rootId) => state.data.buffers.find(buffer =>
    buffer.path === 'draft.py' && (buffer.rootId ?? rootId) === rootId && buffer.content === dirtyText)
  const visibleEditor = () => page.locator('[data-rainy-monaco] .monaco-editor:visible').first()
  const visibleText = async () => (await visibleEditor().locator('.view-lines').innerText()).replace(/\s+/gu, ' ').trim()
  const assertRecoveryState = (state, rootId) => {
    assert.equal(state.data.activePath, 'draft.py')
    assert.equal(state.data.activeRootId ?? rootId, rootId)
    assert(state.data.tabs.some(tab => tab.kind === 'file' && tab.path === 'draft.py' && (tab.rootId ?? rootId) === rootId))
    assert(matchingDirtyBuffer(state, rootId), 'The primary-root file must retain its exact unsaved text')
  }
  if (phase === 'initial') {
    assert.deepEqual(await api({ op: 'workspaces.list' }), [])
    assert.equal((await api({ op: 'state.selection.read' })).workspaceId, null)
    report.checks.push('fresh installed Windows profile has no automatic desktop project')
    await mkdir(primary, { recursive: true })
    await writeFile(join(primary, 'draft.py'), originalText)
    await page.locator('[data-rainy-topbar]').getByRole('button', { name: '文件', exact: true }).click()
    await page.getByRole('menuitem', { name: '浏览当前执行环境文件夹', exact: true }).click()
    const picker = page.getByRole('dialog', { name: '选择工作区目录', exact: true })
    await picker.getByRole('button', { name: '编辑路径', exact: true }).click()
    await picker.getByRole('textbox', { name: '编辑路径', exact: true }).fill(primary)
    await picker.getByRole('textbox', { name: '编辑路径', exact: true }).press('Enter')
    await picker.getByRole('button', { name: '打开', exact: true }).click()
    await picker.waitFor({ state: 'hidden' })
    const { workspaceId, rootId } = await selectedProject()
    await page.getByRole('tree', { name: '文件', exact: true }).getByText('draft.py', { exact: true }).click()
    await visibleEditor().waitFor({ timeout: 60000 })
    await until(async () => (await visibleText()).includes('value = 1'), 'Monaco did not finish displaying the source file')
    await visibleEditor().locator('.native-edit-context, textarea.inputarea').first().focus()
    await page.keyboard.press('Control+a')
    await page.keyboard.insertText(dirtyText)
    await until(async () => matchingDirtyBuffer(await api({ op: 'state.read', workspaceId }), rootId), 'Dirty Monaco buffer was not durably saved')
    assertRecoveryState(await api({ op: 'state.read', workspaceId }), rootId)
    await page.getByRole('tab', { name: '● draft.py', exact: true }).waitFor()
    assert.equal(await readFile(join(primary, 'draft.py'), 'utf8'), originalText)
    report.checks.push('Chinese project opens; actual Monaco unsaved text reaches recovery storage without overwriting the source file')
    await page.screenshot({ path: join(evidence, '01-unsaved-editor.png') }); report.screenshots.push('01-unsaved-editor.png')
    await page.locator('[data-rainy-topbar]').getByRole('button', { name: '设置', exact: true }).click()
    const settings = page.getByRole('dialog', { name: '设置', exact: true })
    assert.equal(await settings.getByRole('button', { name: '授权', exact: true }).count(), 0)
    await settings.getByRole('button', { name: '通用设置', exact: true }).click()
    await settings.getByText(`当前版本：${version}`, { exact: true }).waitFor()
    await page.screenshot({ path: join(evidence, '02-general-version.png') }); report.screenshots.push('02-general-version.png')
    report.checks.push(`General Settings visibly reports application version ${version}`)
    await settings.getByRole('button', { name: '模型', exact: true }).click()
    const strataCard = settings.locator('[data-rainy-strata]')
    await strataCard.locator('summary').first().click()
    await strataCard.getByText('内置 Strata 与 Python 已就绪', { exact: false }).waitFor()
    assert((await strataCard.innerText()).includes('0.1.39'))
    assert.equal(await strataCard.getByRole('button', { name: '启动本地模型', exact: true }).isDisabled(), true)
    await page.screenshot({ path: join(evidence, '02-strata-settings.png') }); report.screenshots.push('02-strata-settings.png')
    report.checks.push('the actual Models settings card shows the bundled Strata runtime and keeps startup disabled without weights')
    await settings.getByRole('button', { name: '运行环境', exact: true }).click()
    const actualChoice = settings.locator('[data-rainy-settings="runtime"]').getByRole('button', { name: '执行环境', exact: true })
    await actualChoice.waitFor()
    assert.equal((await actualChoice.textContent()).trim(), 'Windows')
    assert.equal(await page.getByRole('dialog').count(), 1)
    await page.screenshot({ path: join(evidence, '03-runtime-settings.png') }); report.screenshots.push('03-runtime-settings.png')
    report.checks.push('one settings dialog uses the real runtime bridge without activation controls')
    await page.keyboard.press('Escape')
    assert.equal(await readFile(join(primary, 'draft.py'), 'utf8'), originalText)
    assertRecoveryState(await api({ op: 'state.read', workspaceId }), rootId)
    report.workspaceId = workspaceId
  } else {
    const { workspaceId, rootId } = await selectedProject()
    const state = await api({ op: 'state.read', workspaceId })
    assertRecoveryState(state, rootId)
    await visibleEditor().waitFor({ timeout: 60000 })
    await until(async () => (await visibleText()).includes('value = 41') && (await visibleText()).includes('print(value + 1)'),
      'Monaco did not restore the visible unsaved source text')
    await page.getByRole('tab', { name: '● draft.py', exact: true }).waitFor()
    assert.equal(await readFile(join(primary, 'draft.py'), 'utf8'), originalText)
    await page.screenshot({ path: join(evidence, '04-restarted-dirty-editor.png') }); report.screenshots.push('04-restarted-dirty-editor.png')
    report.checks.push('full carrier restart restores the selected project and unsaved Monaco buffer while preserving file bytes')
  }
  assert.deepEqual(report.pageErrors, [])
  assert(!report.consoleErrors.some(message => message.includes('slot entry crashed')), 'A workspace view failed inside its error boundary')
  assert.deepEqual(await activationBytes(), priorActivation)
  report.checks.push(priorActivation === null ? 'startup creates no activation file' : 'an existing activation file is ignored and preserved byte-for-byte')
  report.passed = true
  await page.evaluate(() => { window.close() })
} catch (error) {
  report.passed = false; report.error = String(error)
  if (page) await page.screenshot({ path: join(evidence, `${phase}-failure.png`) }).catch(() => {})
  throw error
} finally {
  await writeFile(join(evidence, `${phase}.json`), JSON.stringify(report, null, 2) + '\n')
  if (page && !page.isClosed()) await page.evaluate(() => { window.close() }).catch(() => {})
  await browser.close()
}
console.log(JSON.stringify({ passed: report.passed, checks: report.checks, evidence }))
