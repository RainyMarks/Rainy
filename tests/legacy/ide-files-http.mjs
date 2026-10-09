/** Real authenticated filename search and read-only file previews through an isolated packaged WSL Host. */
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { promisify } from 'node:util'
import { openWorkbenchHarness } from './icesky-browser-harness.mjs'

const [runtime, distro, user] = process.argv.slice(2)
assert(runtime && distro && user, 'Pass a prepared Linux runtime, isolated distribution and test user')
const reportRoot = resolve('apps/rainy-desktop/validation/ide-runtime/files-search-preview')
await mkdir(reportRoot, { recursive: true })
const home = `/var/tmp/rainy-ide-files-http-${randomUUID()}`
const harness = await openWorkbenchHarness({ runtime, distro, user, home })
const run = promisify(execFile)
const results = []
const bundleSha256 = createHash('sha256').update(await readFile(resolve('apps/rainy-desktop/lib/ide.js'))).digest('hex')
const report = { runtime, distro, user, home, bundleSha256, results }

async function python(source, ...args) {
  const result = await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'python3', '-c', source, ...args], { windowsHide: true })
  return JSON.parse(result.stdout)
}

async function api(body, expectedStatus = 200) {
  const response = await harness.context.request.post(`${harness.origin}/rainy/ide`, { data: body })
  const result = await response.json()
  assert.equal(response.status(), expectedStatus, JSON.stringify(result))
  assert.equal(result.ok, expectedStatus === 200, JSON.stringify(result))
  return result.ok ? result.value : result.error
}

try {
  const copied = await python('import hashlib,json,sys; print(json.dumps(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest()))', `${runtime}/app/lib/ide.js`)
  assert.equal(copied, bundleSha256)
  const unauthenticated = await fetch(`${harness.origin}/rainy/ide`, { method: 'POST', body: JSON.stringify({ op: 'workspaces.list' }) })
  assert([401, 403].includes(unauthenticated.status))
  const root = `${home}/files-fixture`
  const fixture = await python(String.raw`
import hashlib,json,sys
from pathlib import Path
root=Path(sys.argv[1]); root.mkdir()
(root/'src'/'nested').mkdir(parents=True)
(root/'src'/'nested'/'Main.py').write_text('print("nested")\n')
for excluded in ['.git','node_modules','.venv','__pycache__']:
    (root/excluded).mkdir()
    (root/excluded/'hidden.py').write_text('excluded')
(root/'many').mkdir()
for number in range(205): (root/'many'/f'item-{number:03}.txt').write_text('')
(root/'inside-alias').symlink_to(root/'src',target_is_directory=True)
outside=root.parent/'outside-fixture'; outside.mkdir()
(outside/'outside.py').write_text('outside')
(root/'outside-alias').symlink_to(outside,target_is_directory=True)
large=('中🙂文\r\n'*600000).encode('utf-8')
binary=bytes(range(256))*32
(root/'large-unicode.txt').write_bytes(large)
(root/'binary.bin').write_bytes(binary)
(root/'unsupported.txt').write_bytes(bytes([0xff,0xfe,0x61,0]))
print(json.dumps({'largeBytes':len(large),'largeSha256':hashlib.sha256(large).hexdigest(),'binaryBytes':len(binary),'binarySha256':hashlib.sha256(binary).hexdigest()}))
`, root)
  const workspace = await api({ op: 'workspaces.open', path: root })
  const workspaceId = workspace.workspaceId
  const large = await api({ op: 'files.read', workspaceId, path: 'large-unicode.txt' })
  assert.equal(large.content, null)
  assert.equal(large.readOnlyReason, 'too-large')
  assert.equal(large.bytes, fixture.largeBytes)
  assert.equal(large.preview.kind, 'utf8')
  assert.equal(large.preview.bytesRead, 64 * 1024)
  assert.equal(large.preview.truncated, true)
  assert.equal(large.preview.text.includes('\uFFFD'), false)
  assert.equal(large.preview.text.isWellFormed(), true)
  assert(large.preview.text.startsWith('中🙂文\r\n'))
  assert(Buffer.byteLength(large.preview.text, 'utf8') <= large.preview.bytesRead)
  assert.equal((await api({ op: 'files.save', workspaceId, path: large.path, expectedVersion: large.version, content: 'overwrite' }, 400)).code, 'read-only')
  const binary = await api({ op: 'files.read', workspaceId, path: 'binary.bin' })
  assert.equal(binary.content, null)
  assert.equal(binary.readOnlyReason, 'binary')
  assert.equal(binary.preview.kind, 'hex')
  assert.equal(binary.preview.bytesRead, 4 * 1024)
  assert.equal(binary.preview.truncated, true)
  assert(binary.preview.text.startsWith('00000000  00 01 02 03'))
  assert.equal((await api({ op: 'files.save', workspaceId, path: binary.path, expectedVersion: binary.version, content: 'overwrite' }, 400)).code, 'read-only')
  const unsupported = await api({ op: 'files.read', workspaceId, path: 'unsupported.txt' })
  assert.equal(unsupported.content, null)
  assert.equal(unsupported.readOnlyReason, 'unsupported-encoding')
  assert.equal(unsupported.preview.kind, 'hex')
  assert.equal(unsupported.preview.bytesRead, 4)
  assert.equal(unsupported.preview.truncated, false)
  const unchanged = await python(String.raw`
import hashlib,json,sys
from pathlib import Path
root=Path(sys.argv[1])
print(json.dumps({name:hashlib.sha256((root/name).read_bytes()).hexdigest() for name in ['large-unicode.txt','binary.bin']}))
`, root)
  assert.equal(unchanged['large-unicode.txt'], fixture.largeSha256)
  assert.equal(unchanged['binary.bin'], fixture.binarySha256)
  results.push({ name: 'bounded large Unicode, binary and unsupported-encoding previews reject saving and preserve originals', passed: true,
    largeBytes: large.bytes, utf8PreviewBytes: large.preview.bytesRead, binaryBytes: binary.bytes, hexPreviewBytes: binary.preview.bytesRead })

  assert.deepEqual(await api({ op: 'files.search', workspaceId, query: 'SRC .PY' }), { paths: ['src/nested/Main.py'], truncated: false })
  assert.deepEqual(await api({ op: 'files.search', workspaceId, query: '.py' }), { paths: ['src/nested/Main.py'], truncated: false })
  assert.deepEqual(await api({ op: 'files.search', workspaceId, query: 'hidden' }), { paths: [], truncated: false })
  const bounded = await api({ op: 'files.search', workspaceId, query: 'many item', limit: 999 })
  assert.equal(bounded.paths.length, 200)
  assert.equal(bounded.truncated, true)
  const requested = await api({ op: 'files.search', workspaceId, query: 'many item', limit: 3 })
  assert.equal(requested.paths.length, 3)
  assert.equal(requested.truncated, true)
  assert.equal((await api({ op: 'files.search', workspaceId, query: 'x', limit: 0 }, 400)).code, 'invalid-request')
  results.push({ name: 'unexpanded nested path search excludes dependency directories and both internal and outside directory symlinks', passed: true })
  results.push({ name: 'result cap, caller limit and invalid JSON limit are enforced by the real route', passed: true, resultCap: bounded.paths.length })

  const windowsRoot = resolve(reportRoot, `Windows 中文 project ${randomUUID()}`)
  await mkdir(resolve(windowsRoot, '嵌套 folder'), { recursive: true })
  await writeFile(resolve(windowsRoot, '嵌套 folder', 'script.py'), 'print("Windows → WSL")\n')
  await writeFile(resolve(windowsRoot, 'bytes.bin'), Buffer.from([0, 65, 66, 67]))
  const mapped = (await run('wsl.exe', ['-d', distro, '-u', user, '--exec', 'wslpath', '-u', windowsRoot], { windowsHide: true })).stdout.trim()
  const windowsWorkspace = await api({ op: 'workspaces.open', path: mapped })
  assert.deepEqual(await api({ op: 'files.search', workspaceId: windowsWorkspace.workspaceId, query: 'folder .py' }), { paths: ['嵌套 folder/script.py'], truncated: false })
  const windowsBinary = await api({ op: 'files.read', workspaceId: windowsWorkspace.workspaceId, path: 'bytes.bin' })
  assert.equal(windowsBinary.content, null)
  assert.equal(windowsBinary.preview.kind, 'hex')
  assert.equal(windowsBinary.preview.truncated, false)
  assert(windowsBinary.preview.text.includes('|.ABC|'))
  results.push({ name: 'real Windows Unicode and space directory maps into WSL and supports nested search plus binary preview', passed: true, mappedPath: mapped })
  assert.deepEqual(harness.blocked, [])
  await writeFile(resolve(reportRoot, 'acceptance.json'), JSON.stringify({ ...report, passed: true }, null, 2) + '\n')
  console.log(JSON.stringify({ passed: true, results }))
} catch (error) {
  await writeFile(resolve(reportRoot, 'acceptance.json'), JSON.stringify({ ...report, passed: false, error: String(error) }, null, 2) + '\n')
  throw error
} finally { await harness.stop() }
