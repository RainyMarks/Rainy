/** First-message Session creation, normal attachments, and project/chat ownership in the packaged profile. */
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { promisify } from 'node:util'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro, user] = process.argv.slice(2)
assert(runtime && distro && user)
const output = resolve('apps/rainy-desktop/validation/ide-runtime/workspace-ui')
await mkdir(output, { recursive: true })
const home = `/var/tmp/rainy-first-message-${randomUUID()}`
const run = promisify(execFile)
const harness = await openWorkbenchHarness({ runtime, distro, user, home, fixture: true,
  viewport: { width: 1500, height: 1000 } })
const page = await harness.context.newPage()
page.setDefaultTimeout(30000)
const report = { passed: false, runtime, distro, user, home, checks: [], errors: [] }
const reportPath = resolve(output, 'first-message-acceptance.json')
page.on('pageerror', error => report.errors.push(error.message))
page.on('console', message => { if (message.type() === 'error') report.errors.push(message.text()) })
const topbar = page.locator('[data-rainy-topbar]')
const agent = page.getByRole('complementary', { name: /^(AI assistant|AI 助手)$/ })
const deferred = page.locator('[data-deferred-composer]')
const selector = page.getByRole('combobox', { name: /^(Workspace|工作区)$/ })

async function record(name, details = {}) {
  report.checks.push({ name, passed: true, ...details })
  await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify(report.checks.at(-1)))
}

async function api(body) {
  const response = await harness.context.request.post(`${harness.origin}/rainy/ide`, { data: body })
  const result = await response.json()
  assert.equal(response.status(), 200, JSON.stringify(result))
  assert.equal(result.ok, true, JSON.stringify(result))
  return result.value
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

async function sessions() {
  const result = await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'python3', '-c', String.raw`
import json,sys
from pathlib import Path
out=[]
for path in (Path(sys.argv[1])/'sessions').rglob('session.v4.jsonl'):
    events=[json.loads(line) for line in path.read_text().splitlines()]
    header=events[0]
    out.append({'id':header['id'],'cwd':header['cwd'],
      'users':[event['data'] for event in events if event['type']=='user/message'],
      'assistant':[event['data']['message'] for event in events if event['type']=='assistant/message'],
      'ended':sum(event['type']=='turn/end' for event in events)})
print(json.dumps(out))
`, home], { windowsHide: true })
  return JSON.parse(result.stdout)
}

async function expectCount(count) {
  const current = await sessions()
  assert.equal(current.length, count, JSON.stringify(current.map(session => ({ id: session.id, cwd: session.cwd }))))
  return current
}

async function selected(id) {
  await until(async () => await selector.inputValue() === id && !await selector.isDisabled()
    && (await api({ op: 'state.selection.read' })).workspaceId === id, 'Project selection did not settle')
}

async function select(id) {
  await selector.selectOption(id)
  await selected(id)
}

async function picker(path) {
  await topbar.getByRole('button', { name: /^(File|文件)$/ }).click()
  await page.getByRole('menuitem', { name: /^(Browse WSL folders|浏览 WSL 文件夹)$/ }).click()
  const dialog = page.getByRole('dialog', { name: /^(Select Workspace Directory|选择工作区目录)$/ })
  await dialog.getByRole('button', { name: /^(Edit path|编辑路径)$/ }).click()
  await dialog.getByRole('textbox').fill(path)
  await dialog.getByRole('textbox').press('Enter')
  await until(() => dialog.getByRole('button', { name: /^(Open|打开)$/ }).isEnabled(), 'Picker path did not resolve')
  return dialog
}

async function newChat() {
  await agent.getByRole('button', { name: /^(New chat|New conversation|New Session|新聊天|新会话|新建对话)$/ }).click()
  await deferred.waitFor()
}

try {
  await expectCount(10)
  const workspaceA = (await api({ op: 'workspaces.list' }))[0]
  await page.goto(harness.origin, { waitUntil: 'load' })
  const welcome = page.locator('[class*="onboardingOverlay"]')
  if (await welcome.count()) await welcome.getByRole('button').click()
  await topbar.waitFor({ timeout: 60000 })
  await deferred.waitFor()
  await expectCount(10)
  await select(workspaceA.workspaceId)
  await expectCount(10)
  let dialog = await picker(workspaceA.path)
  await dialog.getByRole('button', { name: /^(Open|打开)$/ }).click()
  await selected(workspaceA.workspaceId)
  await expectCount(10)
  dialog = await picker(home)
  await dialog.getByRole('button', { name: /^(New folder|新建文件夹)$/ }).click()
  const create = page.getByRole('dialog', { name: /^(New folder|新建文件夹)$/ })
  await create.getByRole('textbox', { name: /^(Folder name|文件夹名称)$/ }).fill('Deferred B 中文')
  await create.getByRole('button', { name: /^(Create|创建)$/ }).click()
  await create.waitFor({ state: 'hidden' })
  await dialog.getByRole('button', { name: /^(Open|打开)$/ }).click()
  const workspaceB = await until(async () => (await api({ op: 'workspaces.list' })).find(item => item.path === `${home}/Deferred B 中文`),
    'New project was not registered')
  await selected(workspaceB.workspaceId)
  await expectCount(10)
  await record('Startup, project selection, duplicate folder opening and WSL folder creation retain the ten fixture Sessions without an empty Session')

  await agent.getByRole('button', { name: /^(Choose workspace|选择工作区)$/ }).click()
  await page.getByRole('menuitem').filter({ hasText: workspaceA.title }).first().click()
  await selected(workspaceA.workspaceId)
  await deferred.waitFor()
  await expectCount(10)
  await record('The existing hero workspace picker selects the IDE project without creating a Session')

  const prompt = `FIRST_MESSAGE_${randomUUID()}`
  const attachmentName = 'first-message.txt'
  const attachmentText = 'FIRST_ATTACHMENT_CONTENT\n'
  const input = deferred.getByRole('textbox')
  await input.fill(prompt)
  await deferred.locator('input[type="file"]').setInputFiles({ name: attachmentName, mimeType: 'text/plain', buffer: Buffer.from(attachmentText) })
  await deferred.getByRole('button', { name: new RegExp(attachmentName) }).waitFor()
  await expectCount(10)
  await record('Typing and picking a browser file before Send creates no Session and starts no normal composer')
  await input.focus()
  await page.keyboard.press('Enter')
  await page.keyboard.press('Enter')
  await deferred.waitFor({ state: 'hidden' })
  await agent.getByText('Fixture ready', { exact: true }).waitFor()
  const first = await until(async () => {
    const current = await sessions()
    const session = current.find(item => item.users.some(message => JSON.stringify(message).includes(prompt)))
    return current.length === 11 && session?.ended === 1 && session.assistant.length === 1 ? session : undefined
  }, 'The first message did not complete one ordinary persisted turn')
  assert.equal(first.cwd, workspaceA.path)
  assert.equal(first.users.length, 1)
  const fileBlock = first.users[0].content.find(block => block.type === 'file')
  assert(fileBlock, 'The normal user message omitted its selected attachment')
  assert.deepEqual(fileBlock.attachment, { name: attachmentName, bytes: Buffer.byteLength(attachmentText),
    attachmentId: `sha256:${createHash('sha256').update(attachmentText).digest('hex')}` })
  await expectCount(11)
  await record('Two rapid Enter gestures create exactly one Session with one normal user message, selected file attachment and completed fixture reply',
    { sessionId: first.id, cwd: first.cwd, userMessages: first.users.length, assistantMessages: first.assistant.length, endedTurns: first.ended })
  await until(async () => (await api({ op: 'state.read', workspaceId: workspaceA.workspaceId })).data.lastSessionId === first.id,
    'Project A did not remember its newly submitted chat')

  await select(workspaceB.workspaceId)
  await deferred.waitFor()
  assert.equal((await api({ op: 'state.read', workspaceId: workspaceB.workspaceId })).data.lastSessionId, null)
  await agent.getByRole('button', { name: /^(Chat history|对话历史)$/ }).click()
  await page.locator(`[data-row-key="session:${first.id}"]`).click()
  await agent.getByText(prompt, { exact: true }).waitFor()
  assert.equal(await selector.inputValue(), workspaceB.workspaceId)
  assert.equal((await api({ op: 'state.read', workspaceId: workspaceB.workspaceId })).data.lastSessionId, null)
  await select(workspaceA.workspaceId)
  await agent.getByText(prompt, { exact: true }).waitFor()
  await select(workspaceB.workspaceId)
  await deferred.waitFor()
  await select(workspaceA.workspaceId)
  await agent.getByText(prompt, { exact: true }).waitFor()
  await record('Project B without a saved chat stays unselected; viewing an A chat leaves B and its saved reference unchanged; returning to A restores its own chat')
  const history = page.getByRole('complementary', { name: /^(Workspace|工作区)$/ })
  const groupB = page.locator(`[data-row-key="workspace:${workspaceB.workspaceId}"]`)
  await groupB.hover()
  await groupB.getByRole('button', { name: /New session in|中新建会话/ }).click()
  await selected(workspaceB.workspaceId)
  await deferred.waitFor()
  await expectCount(11)
  assert.equal((await api({ op: 'state.read', workspaceId: workspaceA.workspaceId })).data.lastSessionId, first.id)
  await select(workspaceA.workspaceId)
  await agent.getByText(prompt, { exact: true }).waitFor()
  await record('A workspace-row New Session action targets B without creating a Session or clearing A\'s remembered chat')
  await history.getByRole('button', { name: /^(New Session|New session|新会话|新建会话)$/ }).click()
  await deferred.waitFor()
  await expectCount(11)
  await page.locator(`[data-row-key="session:${first.id}"]`).click()
  await agent.getByText(prompt, { exact: true }).waitFor()
  await page.keyboard.press('Control+Alt+n')
  await deferred.waitFor()
  await expectCount(11)
  await page.locator(`[data-row-key="session:${first.id}"]`).click()
  await agent.getByText(prompt, { exact: true }).waitFor()
  await record('The shared History New Session button and its normal keyboard shortcut clear a chat without creating a blank Session')
  await newChat()
  await expectCount(11)
  await until(async () => (await api({ op: 'state.read', workspaceId: workspaceA.workspaceId })).data.lastSessionId === null,
    'New chat did not clear the project chat selection')
  await page.screenshot({ path: resolve(output, 'first-message-new-chat.png') })
  await record('New chat clears the main selection and project reference without creating another empty Session')
  assert.deepEqual(harness.blocked, [])
  assert.deepEqual(report.errors, [])
  report.passed = true
  await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n')
} catch (error) {
  report.error = String(error)
  await page.screenshot({ path: resolve(output, 'first-message-failure.png') }).catch(() => {})
  await writeFile(resolve(output, 'first-message-failure.txt'), `${String(error)}\n${report.errors.join('\n')}\n${await page.locator('body').innerText().catch(() => '')}`)
  await writeFile(reportPath, JSON.stringify(report, null, 2) + '\n')
  throw error
} finally { await harness.stop() }
