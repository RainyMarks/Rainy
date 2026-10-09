/** Real authenticated IDE routes and persistent LSP over the packaged WSL Host. */
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { randomUUID } from 'node:crypto'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import WebSocket from 'ws'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro, user] = process.argv.slice(2)
assert(runtime && distro && user, 'Pass a prepared Linux runtime, isolated distribution and user')
const reportRoot = resolve('apps/rainy-desktop/validation/ide-runtime/language')
await mkdir(reportRoot, { recursive: true })
const harness = await openWorkbenchHarness({ runtime, distro, user })
const results = []
const active = new Set()
const run = promisify(execFile)

async function request(body, expectedStatus = 200) {
  const response = await harness.context.request.post(`${harness.origin}/rainy/ide`, { data: body })
  const result = await response.json()
  assert.equal(response.status(), expectedStatus, JSON.stringify(result))
  if (expectedStatus !== 200) return result
  assert.equal(result.ok, true, JSON.stringify(result))
  return result.value
}

async function languageClient(workspaceId, language) {
  const cookies = await harness.context.cookies(harness.origin)
  const socket = new WebSocket(`${harness.origin.replace('http:', 'ws:')}/rainy/ide/lsp?${new URLSearchParams({ workspaceId, language })}`, {
    headers: { Origin: harness.origin, Cookie: cookies.map(cookie => `${cookie.name}=${cookie.value}`).join('; ') },
  })
  active.add(socket)
  const notifications = []
  const waiting = new Map()
  let next = 0
  socket.on('message', bytes => {
    const message = JSON.parse(bytes.toString())
    if (message.method && 'id' in message) {
      const result = message.method === 'workspace/configuration'
        ? message.params.items.map(item => item.section === 'python' ? { pythonPath: '/usr/bin/python3' }
          : item.section === 'python.analysis' ? { typeCheckingMode: 'basic', diagnosticMode: 'openFilesOnly' } : {}) : null
      socket.send(JSON.stringify({ jsonrpc: '2.0', id: message.id, result }))
    } else if ('id' in message) {
      const pending = waiting.get(message.id)
      if (pending) { waiting.delete(message.id); clearTimeout(pending.timer); message.error ? pending.reject(new Error(JSON.stringify(message.error))) : pending.accept(message.result) }
    } else notifications.push(message)
  })
  socket.on('close', () => {
    active.delete(socket)
    for (const pending of waiting.values()) { clearTimeout(pending.timer); pending.reject(new Error(`LSP socket closed: ${JSON.stringify(notifications.slice(-10))}`)) }
    waiting.clear()
  })
  await new Promise((accept, reject) => { socket.once('open', accept); socket.once('error', reject) })
  const notify = (method, params) => socket.send(JSON.stringify({ jsonrpc: '2.0', method, params }))
  const call = (method, params) => new Promise((accept, reject) => {
    const id = ++next
    const timer = setTimeout(() => { waiting.delete(id); reject(new Error(`${language} ${method} timed out: ${JSON.stringify(notifications.slice(-10))}`)) }, 30000)
    waiting.set(id, { accept, reject, timer })
    socket.send(JSON.stringify({ jsonrpc: '2.0', id, method, params }))
  })
  await call('initialize', { processId: null, rootUri: null, capabilities: {
    workspace: { configuration: true }, textDocument: { synchronization: { dynamicRegistration: false },
      completion: { completionItem: { snippetSupport: false } }, publishDiagnostics: { versionSupport: true } },
  }, initializationOptions: { preferences: { includeCompletionsForModuleExports: true } } })
  notify('initialized', {})
  notify('workspace/didChangeConfiguration', { settings: { python: { pythonPath: '/usr/bin/python3', analysis: { typeCheckingMode: 'basic' } } } })
  return { call, notify, notifications, async close() {
    await call('shutdown', null)
    notify('exit', null)
    await new Promise(accept => { if (socket.readyState === WebSocket.CLOSED) return accept(); socket.once('close', accept); socket.close() })
  } }
}

async function until(read, message) {
  const deadline = Date.now() + 30000
  while (Date.now() < deadline) { const value = read(); if (value) return value; await new Promise(accept => setTimeout(accept, 50)) }
  throw new Error(message)
}

const fixtures = [
  { language: 'python', name: 'sample.py', text: 'def answer(value: int) -> int:\n    return value + 1\nresult = answer(41)\nbad: int = "wrong"\nans\n', use: { line: 2, character: 11 }, completion: { line: 4, character: 3 } },
  { language: 'javascript', name: 'sample.js', text: '// @ts-check\nfunction answer(value) { return value + 1; }\nconst result = answer(41);\nresult.missing();\nans\n', use: { line: 2, character: 17 }, completion: { line: 4, character: 3 } },
  { language: 'typescript', name: 'sample.ts', text: 'function answer(value: number): number { return value + 1; }\nconst result = answer(41);\nconst bad: number = "wrong";\nans\n', use: { line: 1, character: 17 }, completion: { line: 3, character: 3 } },
  { language: 'c', name: 'sample.c', text: 'int answer(int value) { return value + 1; }\nint main(void) {\n  int result = answer(41);\n  int bad = "wrong";\n  ans\n}\n', use: { line: 2, character: 16 }, completion: { line: 4, character: 5 } },
  { language: 'cpp', name: 'sample.cpp', text: 'int answer(int value) { return value + 1; }\nint main() {\n  int result = answer(41);\n  int bad = "wrong";\n  ans\n}\n', use: { line: 2, character: 16 }, completion: { line: 4, character: 5 } },
]

try {
  const unauthenticated = await fetch(`${harness.origin}/rainy/ide`, { method: 'POST', body: JSON.stringify({ op: 'workspaces.list' }) })
  assert([401, 403].includes(unauthenticated.status))
  const asset = await harness.context.request.get(`${harness.origin}/rainy/editor/editor.js`)
  assert.equal(asset.status(), 200)
  assert.match(asset.headers()['content-type'], /javascript/)
  const tools = await request({ op: 'tools.status' })
  assert.equal(tools.tools.filter(tool => !tool.ready).length, 0, JSON.stringify(tools))
  const parent = (await request({ op: 'workspaces.list' }))[0]
  assert(parent, 'The real profile must initialize its default workspace')
  const child = `Rainy IDE 中文 ${randomUUID()}`
  await request({ op: 'files.mkdir', workspaceId: parent.workspaceId, path: child })
  const workspace = await request({ op: 'workspaces.open', path: `${parent.path}/${child}` })
  assert.equal((await request({ op: 'workspaces.open', path: workspace.path })).workspaceId, workspace.workspaceId)
  const windowsDirectory = resolve(reportRoot, `Windows 中文 project ${randomUUID()}`)
  await mkdir(windowsDirectory)
  const mapped = (await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'wslpath', '-u', windowsDirectory], { windowsHide: true })).stdout.trim()
  const windowsWorkspace = await request({ op: 'workspaces.open', path: mapped })
  assert.equal((await request({ op: 'workspaces.open', path: `${mapped}/` })).workspaceId, windowsWorkspace.workspaceId)
  await request({ op: 'files.create', workspaceId: windowsWorkspace.workspaceId, path: '路径 test.txt', content: 'Windows → WSL\n' })
  assert.equal(await (await import('node:fs/promises')).readFile(resolve(windowsDirectory, '路径 test.txt'), 'utf8'), 'Windows → WSL\n')
  results.push({ name: 'Windows Unicode/space directory mapping, duplicate opening and real file creation', passed: true })
  const workspaceId = workspace.workspaceId
  const state = await request({ op: 'state.read', workspaceId })
  assert.equal(state.data.layout.sidebarWidth, 240)
  assert.equal(state.data.layout.agentWidth, 400)
  const file = await request({ op: 'files.create', workspaceId, path: 'BOM.txt', content: '\uFEFFone\r\ntwo\r\n' })
  assert.equal(file.bom, true)
  const changed = await request({ op: 'files.save', workspaceId, path: 'BOM.txt', content: 'one\ntwo changed', expectedVersion: file.version })
  assert.equal(changed.content, 'one\r\ntwo changed')
  assert.equal((await request({ op: 'files.save', workspaceId, path: 'BOM.txt', content: 'stale', expectedVersion: file.version }, 409)).error.code, 'version-conflict')
  results.push({ name: 'authentication, editor asset, tool readiness, Unicode workspace deduplication and versioned BOM/CRLF save', passed: true })
  for (const fixture of fixtures) {
    await request({ op: 'files.create', workspaceId, path: fixture.name, content: '' })
    const client = await languageClient(workspaceId, fixture.language)
    const uri = new URL(`file://${workspace.path}/${fixture.name}`).href
    try {
      client.notify('textDocument/didOpen', { textDocument: { uri, languageId: fixture.language, version: 1, text: fixture.text } })
      const diagnostic = await until(() => client.notifications.find(message => message.method === 'textDocument/publishDiagnostics' && message.params.uri === uri && message.params.diagnostics.length), `${fixture.language}: unsaved diagnostics missing`)
      const completion = await client.call('textDocument/completion', { textDocument: { uri }, position: fixture.completion })
      const items = Array.isArray(completion) ? completion : completion?.items
      assert(items?.some(item => item.label.includes('answer')), `${fixture.language}: answer completion missing`)
      const location = await client.call('textDocument/definition', { textDocument: { uri }, position: fixture.use })
      assert(location && (!Array.isArray(location) || location.length), `${fixture.language}: definition missing`)
      const references = await client.call('textDocument/references', { textDocument: { uri }, position: fixture.use, context: { includeDeclaration: true } })
      assert(references?.length >= 2, `${fixture.language}: references missing`)
      const rename = await client.call('textDocument/rename', { textDocument: { uri }, position: fixture.use, newName: 'answerRenamed' })
      assert(rename && (Object.keys(rename.changes ?? {}).length || rename.documentChanges?.length), `${fixture.language}: rename edits missing`)
      assert.equal((await request({ op: 'files.read', workspaceId, path: fixture.name })).content, '')
      results.push({ language: fixture.language, passed: true, diagnostics: diagnostic.params.diagnostics.length, completion: true, definition: true, references: references.length, rename: true, diskUnchanged: true })
      console.log(JSON.stringify(results.at(-1)))
    } finally { await client.close() }
  }
  for (const [language, path, text] of [['python', 'format.py', 'x=  1\r\n'], ['typescript', 'format.ts', 'const x:number=1\r\n'], ['cpp', 'format.cpp', 'int main(){return 0;}\r\n']]) {
    const formatted = await request({ op: 'format', workspaceId, language, path, text })
    assert.notEqual(formatted.text, text)
    assert.match(formatted.text, /\r\n/)
    results.push({ name: `${language} maintained formatter preserves CRLF`, passed: true })
  }
  const quiescence = await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'python3', '-c', `
import json,os,sys,time
from pathlib import Path
root=sys.argv[1]
for attempt in range(100):
    remaining=[]
    for process in Path('/proc').iterdir():
        if not process.name.isdigit(): continue
        try:
            cwd=os.readlink(process/'cwd')
            if cwd==root or cwd.startswith(root+'/'): remaining.append(int(process.name))
        except (FileNotFoundError,PermissionError,ProcessLookupError): pass
    if not remaining: break
    time.sleep(0.05)
print(json.dumps({'remainingProcesses':remaining}))
if remaining: raise SystemExit(1)
`, workspace.path], { windowsHide: true })
  assert.deepEqual(JSON.parse(quiescence.stdout).remainingProcesses, [])
  results.push({ name: 'all language and formatter processes exited after their owning connections completed', passed: true })
  assert.deepEqual(harness.blocked, [])
  await writeFile(resolve(reportRoot, 'acceptance.json'), JSON.stringify({ passed: true, runtime, distro, results }, null, 2) + '\n')
} catch (error) {
  await writeFile(resolve(reportRoot, 'acceptance.json'), JSON.stringify({ passed: false, runtime, distro, results, error: String(error) }, null, 2) + '\n')
  throw error
} finally {
  for (const socket of active) socket.terminate()
  await harness.stop()
}
