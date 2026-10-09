/** Real file, workspace, conflict and recovery interactions in a private packaged WSL profile. */
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { promisify } from 'node:util'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro, user] = process.argv.slice(2)
const resumeRecovery = process.argv.includes('--resume-recovery')
const filesOnly = process.argv.includes('--files-only') || resumeRecovery
const pickerOnly = process.argv.includes('--picker-only')
assert(!(filesOnly && pickerOnly), 'Choose one scoped run')
assert(runtime && distro && user, 'Pass the prepared runtime, isolated distribution and test user')
const output = resolve('apps/rainy-desktop/validation/ide-runtime/workspace-ui')
await mkdir(output, { recursive: true })
const reportPath = resolve(output, pickerOnly ? 'picker-acceptance.json' : 'acceptance.json')
const previous = resumeRecovery ? JSON.parse(await readFile(resolve(output, 'acceptance.json'), 'utf8')) : undefined
const home = previous?.home ?? `/var/tmp/rainy-workspace-ui-${randomUUID()}`
const report = { runtime, distro, user, home,
  scope: pickerOnly ? 'directory picker flows' : filesOnly ? 'file and recovery flows; directory picker omitted' : 'complete workspace UI',
  checks: previous?.checks ?? [], errors: [] }
const run = promisify(execFile)
let harness
let page

async function record(name, details = {}) {
  report.checks.push({ name, passed: true, ...details })
  await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify(report.checks.at(-1)))
}

async function start() {
  harness = await openWorkbenchHarness({ runtime, distro, user, home, fixture: true, viewport: { width: 1500, height: 1000 } })
  page = await harness.context.newPage()
  page.setDefaultTimeout(30000)
  page.on('pageerror', error => report.errors.push(error.message))
  await page.goto(harness.origin, { waitUntil: 'load' })
  const welcome = page.locator('[class*="onboardingOverlay"]')
  if (await welcome.count()) await welcome.getByRole('button').click()
  await page.locator('[data-rainy-ide]').waitFor({ timeout: 60000 })
}

async function api(body, status = 200) {
  const response = await harness.context.request.post(`${harness.origin}/rainy/ide`, { data: body })
  const result = await response.json()
  assert.equal(response.status(), status, JSON.stringify(result))
  assert.equal(result.ok, status === 200, JSON.stringify(result))
  return result.ok ? result.value : result.error
}

async function until(read, message) {
  const deadline = Date.now() + 30000
  while (Date.now() < deadline) {
    const value = await read()
    if (value) return value
    await new Promise(accept => setTimeout(accept, 75))
  }
  throw new Error(message)
}

async function requestFromUi(op, action, status = 200) {
  const pending = page.waitForResponse(response => response.url().endsWith('/rainy/ide')
    && response.request().method() === 'POST' && response.request().postDataJSON()?.op === op)
  await action()
  const response = await pending
  const result = await response.json()
  assert.equal(response.status(), status, JSON.stringify(result))
  assert.equal(result.ok, status === 200, JSON.stringify(result))
  return result.ok ? result.value : result.error
}

const workspaceSelect = () => page.getByRole('combobox', { name: /^(Workspace|工作区)$/ })
const editorLines = () => page.locator('[data-rainy-monaco] .view-lines:visible')
const main = () => page.getByRole('main')
const explorer = () => page.getByRole('tree', { name: /^(Files|文件)$/ })

async function selectWorkspace(id) {
  await workspaceSelect().selectOption(id)
  await until(async () => await workspaceSelect().inputValue() === id && !await workspaceSelect().isDisabled(), 'Workspace selection did not settle')
  await explorer().waitFor({ state: 'visible' })
  await until(async () => (await api({ op: 'state.selection.read' })).workspaceId === id, 'Workspace selection did not persist in the Host')
}

async function menuItem(name) {
  await page.locator('[data-rainy-topbar]').getByRole('button', { name: /^(File|文件)$/ }).click()
  await page.getByRole('menuitem', { name }).click()
}

async function promptPath(name, path, op) {
  const dialog = page.getByRole('dialog', { name, exact: true })
  await dialog.getByRole('textbox').fill(path)
  const result = await requestFromUi(op, () => dialog.getByRole('button', { name: /^(Confirm|确认)$/ }).click())
  await dialog.waitFor({ state: 'hidden' })
  return result
}

async function createFile(path) {
  await menuItem(/^(New file|新建文件)$/)
  await promptPath(/^(New file|新建文件)$/, path, 'files.create')
  await editorLines().first().waitFor({ state: 'visible', timeout: 60000 })
}

async function replaceEditor(text) {
  await editorLines().first().click({ position: { x: 40, y: 10 } })
  await page.keyboard.press('Control+a')
  await page.keyboard.insertText(text)
}

async function expectEditor(text) {
  await until(async () => (await editorLines().allTextContents()).join('\n').includes(text), `Editor did not show ${text}`)
}

async function openQuick(path) {
  await menuItem(/^(Search workspace files|搜索工作区文件)$/)
  const dialog = page.getByRole('dialog', { name: /^(Search workspace files|搜索工作区文件)$/ })
  await dialog.getByRole('combobox').fill(path)
  await dialog.getByRole('option', { name: path, exact: true }).click()
  await dialog.waitFor({ state: 'hidden' })
}

async function openPicker(path) {
  await menuItem(/^(Browse WSL folders|浏览 WSL 文件夹)$/)
  const dialog = page.getByRole('dialog', { name: /^(Select Workspace Directory|选择工作区目录)$/ })
  await dialog.getByRole('button', { name: /^(Edit path|编辑路径)$/ }).click()
  const input = dialog.getByRole('textbox', { name: /^(Edit path|编辑路径)$/ })
  await input.fill(path)
  await input.press('Enter')
  await input.waitFor({ state: 'hidden' })
  await until(() => dialog.getByRole('button', { name: /^(Open|打开)$/ }).isEnabled(), 'Directory picker did not resolve the entered path')
  return dialog
}

async function externalWrite(root, name, text) {
  await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'python3', '-c',
    'from pathlib import Path; import sys; root=Path(sys.argv[1]).resolve(); target=(root/sys.argv[2]).resolve(); assert target.is_relative_to(root); target.write_text(sys.argv[3],encoding="utf-8")',
    root, name, text], { windowsHide: true })
}

async function savedDraft(workspaceId, path, text) {
  return until(async () => (await api({ op: 'state.read', workspaceId })).data.buffers.find(buffer => buffer.path === path
    && buffer.content.replaceAll('\r\n', '\n') === text.replaceAll('\r\n', '\n')),
    `Recovery copy did not retain ${path}`)
}

async function selectSession(id) {
  await page.getByRole('complementary', { name: /^(AI assistant|AI 助手)$/ })
    .getByRole('button', { name: /^(Chat history|对话历史)$/ }).click()
  const row = page.locator(`[data-row-key="session:${id}"]`)
  if (await row.count() === 0) {
    const expand = page.getByRole('button', { name: /展开其余.*会话|Show.*more sessions|Expand.*sessions/ })
    if (await expand.count()) await expand.first().click()
  }
  await row.click()
  await page.locator('[data-rainy-agent]').getByText('Fixture ready', { exact: true }).waitFor({ state: 'visible' })
  await page.getByRole('tab', { name: /^(Files|文件)$/, exact: true }).first().click()
}

async function workbench(id) {
  await page.locator('[data-rainy-topbar]').getByRole('button', { name: /^CTF/ }).click()
  await page.getByRole('tab', { name: 'IceSky', exact: true }).click()
  const carrier = page.locator('[data-rainy-ctf-workbench] iframe')
  await carrier.waitFor({ state: 'visible' })
  const handle = await carrier.elementHandle()
  const frame = await handle.contentFrame()
  assert(frame)
  await frame.waitForFunction(sessionId => window.app?.toolLoading === false
    && window.IceSkyRuntime?.context?.kind === 'session' && window.IceSkyRuntime.context.id === sessionId, id)
  const all = frame.getByRole('button', { name: /^(All tools|全部工具)$/ })
  if (await all.count()) await all.click()
  await frame.getByRole('tab', { name: /变换|Transform/ }).first().click()
  await frame.locator('#transform-input').waitFor({ state: 'visible' })
  return frame
}

async function saveCtf(frame, text, sessionId) {
  if (await frame.locator('#transform-input').inputValue() === text) return
  const pending = page.waitForResponse(response => response.request().method() === 'PUT'
    && new URL(response.url()).pathname === '/rainy/icesky/state'
    && new URL(response.url()).searchParams.get('scope') === `session:${sessionId}` && response.status() === 200)
  await frame.locator('#transform-input').fill(text)
  await pending
}

try {
  await start()
  const initial = await api({ op: 'workspaces.list' })
  const workspaceA = initial.find(workspace => workspace.path === `${home}/workspace`)
  assert(workspaceA)
  const a = workspaceA.workspaceId
  await selectWorkspace(a)
  let workspaceB
  if (filesOnly) {
    workspaceB = initial.find(workspace => workspace.path === `${home}/Workspace B 中文`)
    if (workspaceB === undefined) {
      await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'mkdir', `${home}/Workspace B 中文`], { windowsHide: true })
      workspaceB = await api({ op: 'workspaces.open', path: `${home}/Workspace B 中文` })
      await page.reload({ waitUntil: 'load' })
      await page.locator('[data-rainy-ide]').waitFor({ timeout: 60000 })
    }
  } else {
  let picker = await openPicker(workspaceA.path)
  await picker.getByRole('button', { name: /^(Cancel|取消)$/ }).click()
  assert.equal((await api({ op: 'workspaces.list' })).length, initial.length)
  picker = await openPicker(workspaceA.path)
  const duplicate = await requestFromUi('workspaces.open', () => picker.getByRole('button', { name: /^(Open|打开)$/ }).click())
  assert.equal(duplicate.workspaceId, a)
  assert.equal((await api({ op: 'workspaces.list' })).length, initial.length)
  picker = await openPicker(home)
  await picker.getByRole('button', { name: /^(New folder|新建文件夹)$/ }).click()
  const createDialog = page.getByRole('dialog', { name: /^(New folder|新建文件夹)$/ })
  await createDialog.getByRole('textbox', { name: /^(Folder name|文件夹名称)$/ }).fill('Workspace B 中文')
  await createDialog.getByRole('button', { name: /^(Create|创建)$/ }).click()
  await createDialog.waitFor({ state: 'hidden' })
  workspaceB = await requestFromUi('workspaces.open', () => picker.getByRole('button', { name: /^(Open|打开)$/ }).click())
  assert.equal(workspaceB.path, `${home}/Workspace B 中文`)
  assert.equal((await api({ op: 'workspaces.list' })).length, initial.length + 1)
  await until(async () => (await api({ op: 'state.selection.read' })).workspaceId === workspaceB.workspaceId,
    'Picker selection did not become the remembered workspace')
  await record('WSL folder picker supports cancellation, duplicate opening and creating a Unicode workspace directory')
  }
  const b = workspaceB.workspaceId

  if (!pickerOnly) {
  await selectWorkspace(a)
  if (!resumeRecovery) {
  await menuItem(/^(New folder|新建文件夹)$/)
  await promptPath(/^(New folder|新建文件夹)$/, 'generated', 'files.mkdir')
  await createFile('generated/note.txt')
  await replaceEditor('SAVED_FROM_UI\n')
  const savedFromUi = await requestFromUi('files.save', () => menuItem(/^(Save|保存)$/))
  assert.equal(savedFromUi.content.replaceAll('\r\n', '\n'), 'SAVED_FROM_UI\n')
  assert.equal((await api({ op: 'files.read', workspaceId: a, path: 'generated/note.txt' })).content, savedFromUi.content)
  const folder = explorer().getByRole('treeitem', { name: 'generated', exact: true })
  if (await folder.getAttribute('aria-expanded') !== 'true') await folder.click()
  await explorer().getByRole('treeitem', { name: 'note.txt', exact: true }).click()
  await page.getByRole('complementary', { name: /^(Workspace|工作区)$/ }).getByTitle(/^(Rename|重命名)$/).click()
  await promptPath(/^(Rename|重命名)$/, 'generated/renamed.txt', 'files.rename')
  await explorer().getByRole('treeitem', { name: 'renamed.txt', exact: true }).click()
  const deleteButton = page.getByRole('complementary', { name: /^(Workspace|工作区)$/ }).getByTitle(/^(Delete|删除)$/)
  await deleteButton.click()
  let deleteDialog = page.getByRole('dialog', { name: /^(Delete|删除)$/ })
  await deleteDialog.getByRole('button', { name: /^(Cancel|取消)$/ }).click()
  assert.equal((await api({ op: 'files.read', workspaceId: a, path: 'generated/renamed.txt' })).content, savedFromUi.content)
  await deleteButton.click()
  deleteDialog = page.getByRole('dialog', { name: /^(Delete|删除)$/ })
  await requestFromUi('files.delete', () => deleteDialog.getByRole('button', { name: /^(Confirm|确认)$/ }).click())
  assert.equal((await api({ op: 'files.list', workspaceId: a, path: 'generated' })).entries.length, 0)
  await createFile('scratch.txt')
  await replaceEditor('UNSAVED_CLOSE_CHECK\n')
  await page.getByRole('button', { name: /^(Close|关闭) scratch\.txt$/ }).click()
  let dirtyDialog = page.getByRole('dialog', { name: /unsaved changes|未保存的修改/ })
  await dirtyDialog.getByRole('button', { name: /^(Cancel|取消)$/ }).click()
  await expectEditor('UNSAVED_CLOSE_CHECK')
  await page.getByRole('button', { name: /^(Close|关闭) scratch\.txt$/ }).click()
  dirtyDialog = page.getByRole('dialog', { name: /unsaved changes|未保存的修改/ })
  await dirtyDialog.getByRole('button', { name: /^(Discard changes|放弃修改)$/ }).click()
  assert.equal((await api({ op: 'files.read', workspaceId: a, path: 'scratch.txt' })).content, '')
  await record('File menu and explorer create, save, rename and delete real files; cancelled deletion and dirty-close choices preserve the correct contents')

  await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'python3', '-c', String.raw`
from pathlib import Path
import sys
root=Path(sys.argv[1]); (root/'src'/'deep').mkdir(parents=True)
(root/'src'/'deep'/'query_match.py').write_text('QUERY_SENTINEL = 1\n')
(root/'binary.bin').write_bytes(bytes(range(256))*32)
(root/'large.txt').write_text('中🙂文\r\n'*600000,encoding='utf-8')
for name,value in [('clean.txt','CLEAN_BASE\n'),('conflict.txt','CONFLICT_BASE\n'),('recovery.txt','RECOVERY_BASE\n')]:
    (root/name).write_text(value)
`, workspaceA.path], { windowsHide: true })
  await openQuick('src/deep/query_match.py')
  await expectEditor('QUERY_SENTINEL')
  assert.equal(await explorer().getByRole('treeitem', { name: 'query_match.py', exact: true }).count(), 0)
  await openQuick('binary.bin')
  await page.getByText(/Binary file \(editing unavailable\)|二进制文件（不可编辑）/).waitFor()
  assert((await main().locator('pre[aria-label]').innerText()).startsWith('00000000  00 01 02 03'))
  assert.equal(await main().getByRole('button', { name: /^(Save|保存)$/ }).isDisabled(), true)
  await openQuick('large.txt')
  await page.getByText(/exceeds the editing limit|文件超过编辑大小限制/).waitFor()
  assert((await main().locator('pre[aria-label]').innerText()).includes('中🙂文'))
  assert((await main().innerText()).includes('65536'))
  await page.screenshot({ path: resolve(output, 'readonly-preview.png') })
  await record('Quick Open finds an unexpanded nested file; binary and oversized Unicode previews render read-only in the actual editor surface')

  await openQuick('clean.txt')
  await expectEditor('CLEAN_BASE')
  await externalWrite(workspaceA.path, 'clean.txt', 'CLEAN_EXTERNAL\n')
  await expectEditor('CLEAN_EXTERNAL')
  await openQuick('conflict.txt')
  await replaceEditor('LOCAL_UNSAVED\n')
  await savedDraft(a, 'conflict.txt', 'LOCAL_UNSAVED\n')
  await externalWrite(workspaceA.path, 'conflict.txt', 'EXTERNAL_ONE\n')
  await page.getByText(/The file on disk changed|磁盘上的文件已变化/).waitFor()
  let conflict = await requestFromUi('files.save', () => main().getByRole('button', { name: /^(Save|保存)$/ }).click(), 409)
  assert.equal(conflict.code, 'version-conflict')
  await page.locator('[data-rainy-monaco] .monaco-diff-editor:visible').waitFor()
  await expectEditor('EXTERNAL_ONE')
  await expectEditor('LOCAL_UNSAVED')
  const review = () => main().getByRole('button', { name: /Use the reviewed disk version|确认使用当前磁盘版本/ })
  await review().click()
  await externalWrite(workspaceA.path, 'conflict.txt', 'EXTERNAL_TWO\n')
  conflict = await requestFromUi('files.save', () => main().getByRole('button', { name: /^(Save|保存)$/ }).click(), 409)
  assert.equal(conflict.code, 'version-conflict')
  assert.equal((await api({ op: 'files.read', workspaceId: a, path: 'conflict.txt' })).content, 'EXTERNAL_TWO\n')
  await expectEditor('EXTERNAL_TWO')
  await page.screenshot({ path: resolve(output, 'external-conflict.png') })
  await review().click()
  await requestFromUi('files.save', () => main().getByRole('button', { name: /^(Save|保存)$/ }).click())
  assert.equal((await api({ op: 'files.read', workspaceId: a, path: 'conflict.txt' })).content, 'LOCAL_UNSAVED\n')
  await record('Clean files refresh after external writes; dirty files keep local text and review a real diff, while a second external write still rejects stale CAS saving')
  }

  await openQuick('recovery.txt')
  const draftA = 'WORKSPACE_A_RECOVERY\n'
  const draftB = 'WORKSPACE_B_RECOVERY\n'
  await replaceEditor(draftA)
  await savedDraft(a, 'recovery.txt', draftA)
  const [firstSession, secondSession] = harness.ready.fixtureSessionIds
  await selectSession(firstSession)
  let frame = await workbench(firstSession)
  await saveCtf(frame, 'WORKSPACE_CTF_ONE', firstSession)
  await selectSession(secondSession)
  frame = await workbench(secondSession)
  assert.notEqual(await frame.locator('#transform-input').inputValue(), 'WORKSPACE_CTF_ONE')
  await saveCtf(frame, 'WORKSPACE_CTF_TWO', secondSession)
  await selectSession(firstSession)
  frame = await workbench(firstSession)
  assert.equal(await frame.locator('#transform-input').inputValue(), 'WORKSPACE_CTF_ONE')
  await page.getByRole('button', { name: /^(Hide CTF tools|收起 CTF 工具)$/ }).click()
  await expectEditor('WORKSPACE_A_RECOVERY')
  await until(async () => (await api({ op: 'state.read', workspaceId: a })).data.lastSessionId === firstSession, 'Workspace did not persist the selected chat reference')
  await selectWorkspace(b)
  const existingB = await api({ op: 'files.list', workspaceId: b, path: '' })
  if (existingB.entries.some(entry => entry.path === 'other.txt')) await openQuick('other.txt')
  else await createFile('other.txt')
  await replaceEditor(draftB)
  await savedDraft(b, 'other.txt', draftB)
  await selectWorkspace(a)
  await expectEditor('WORKSPACE_A_RECOVERY')
  assert.equal((await api({ op: 'files.read', workspaceId: a, path: 'recovery.txt' })).content, 'RECOVERY_BASE\n')
  assert.equal((await api({ op: 'files.read', workspaceId: b, path: 'other.txt' })).content, '')
  frame = await workbench(firstSession)
  assert.equal(await frame.locator('#transform-input').inputValue(), 'WORKSPACE_CTF_ONE')
  await page.getByRole('button', { name: /^(Hide CTF tools|收起 CTF 工具)$/ }).click()
  await expectEditor('WORKSPACE_A_RECOVERY')
  await page.screenshot({ path: resolve(output, 'workspace-drafts.png') })
  await record('Two workspaces retain separate unsaved buffers while switching chats and CTF contexts preserves both editor and tool drafts')

  await page.reload({ waitUntil: 'load' })
  await page.locator('[data-rainy-ide]').waitFor({ timeout: 60000 })
  await until(async () => await workspaceSelect().inputValue() === a, 'Renderer reload did not restore its selected workspace')
  await expectEditor('WORKSPACE_A_RECOVERY')
  await savedDraft(a, 'recovery.txt', draftA)
  frame = await workbench(firstSession)
  assert.equal(await frame.locator('#transform-input').inputValue(), 'WORKSPACE_CTF_ONE')
  await record('Renderer reload restores the selected workspace, open source draft and selected chat CTF draft without modifying disk')

  assert.deepEqual(harness.blocked, [])
  await harness.stop()
  harness = undefined
  await start()
  assert.equal(harness.ready.reusedSessions, 10)
  assert.equal(harness.ready.modelRequests, 0)
  await until(async () => await workspaceSelect().inputValue() === a, 'A new Host origin did not automatically restore the active workspace')
  const automaticWorkspace = await workspaceSelect().inputValue()
  await expectEditor('WORKSPACE_A_RECOVERY')
  await savedDraft(a, 'recovery.txt', draftA)
  frame = await workbench(firstSession)
  assert.equal(await frame.locator('#transform-input').inputValue(), 'WORKSPACE_CTF_ONE')
  await page.getByRole('button', { name: /^(Hide CTF tools|收起 CTF 工具)$/ }).click()
  await selectWorkspace(b)
  await expectEditor('WORKSPACE_B_RECOVERY')
  await savedDraft(b, 'other.txt', draftB)
  assert.equal((await api({ op: 'files.read', workspaceId: a, path: 'recovery.txt' })).content, 'RECOVERY_BASE\n')
  assert.equal((await api({ op: 'files.read', workspaceId: b, path: 'other.txt' })).content, '')
  await page.screenshot({ path: resolve(output, 'host-restart.png') })
  report.checks = report.checks.filter(check => !check.name.startsWith('Same-home Host restart'))
  await record('Same-home Host restart automatically restores the active workspace, both dirty drafts and selected chat tool context',
    { automaticWorkspace, reusedSessions: harness.ready.reusedSessions, newModelRequests: harness.ready.modelRequests })
  }
  assert.deepEqual(harness.blocked, [])
  assert.deepEqual(report.errors, [])
  report.passed = true
  await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n')
} catch (error) {
  report.passed = false
  report.error = String(error)
  if (page) {
    await page.screenshot({ path: resolve(output, 'failure.png') }).catch(() => {})
    await writeFile(resolve(output, 'failure.txt'), `${String(error)}\n${await page.locator('body').innerText().catch(() => '')}`)
  }
  await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n')
  throw error
} finally { await harness?.stop() }
