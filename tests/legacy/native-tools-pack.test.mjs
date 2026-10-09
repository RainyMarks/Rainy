import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { lstat, mkdir, mkdtemp, readFile, readdir, rm, stat, symlink, unlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, relative, resolve, sep } from 'node:path'
import test from 'node:test'
import { zipSync } from 'fflate'
import { create as createTar } from 'tar'
import { childPath, planCopies, preparePack, validateArchiveListing, validateDefinition, verifyPack } from '../scripts/prepare-native-tools.mjs'

async function fixture(t) {
  const root = await mkdtemp(resolve(tmpdir(), 'rainy-native-pack-'))
  t.after(async () => {
    const within = relative(resolve(tmpdir()), root)
    assert.ok(within && !isAbsolute(within) && !within.startsWith(`..${sep}`))
    assert.equal((await lstat(root)).isSymbolicLink(), false)
    await rm(root, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 })
  })
  const sourceRoot = resolve(root, 'source')
  await mkdir(resolve(sourceRoot, 'app/cache'), { recursive: true })
  await writeFile(resolve(sourceRoot, 'app/tool.exe'), 'test program bytes')
  await writeFile(resolve(sourceRoot, 'app/NOTICE.txt'), 'upstream notice')
  await writeFile(resolve(sourceRoot, 'app/cache/private.txt'), 'personal history')
  await writeFile(resolve(sourceRoot, 'app/session.log'), 'personal log')
  const definition = {
    version: 1, runtimes: [], tools: [{
      id: 'sample', category: 'misc', name: 'Sample', version: '1',
      description: { zh: '示例。', en: 'Sample.' }, keywords: ['file'],
      entry: { kind: 'console', path: 'tools/sample/tool.exe', cwd: 'tools/sample' },
      roots: ['tools/sample'], copies: [{ from: 'app', to: 'tools/sample', exclude: ['cache'] }],
    }],
  }
  return { root, sourceRoot, stageRoot: resolve(root, 'stage'), cacheRoot: resolve(root, 'cache'), definition, offline: true }
}

test('preparation preserves source files, excludes personal data, and verifies the copied package', async t => {
  const state = await fixture(t)
  const result = await preparePack(state)
  assert.equal(result.catalog.tools[0].available, true)
  assert.equal(result.copied, 2)
  assert.equal(result.excluded.length, 2)
  assert.equal(await readFile(resolve(state.sourceRoot, 'app/cache/private.txt'), 'utf8'), 'personal history')
  assert.equal(await readFile(resolve(state.stageRoot, 'tools/sample/NOTICE.txt'), 'utf8'), 'upstream notice')
  await assert.rejects(stat(resolve(state.stageRoot, 'tools/sample/cache/private.txt')), { code: 'ENOENT' })
  const verified = await verifyPack(state.stageRoot)
  assert.equal(verified.toolCount, 1)
  assert.equal(verified.availableToolCount, 1)
  assert.equal(verified.files.length, 3)
})

test('an incremental preparation reuses matching files and repairs changed package bytes', async t => {
  const state = await fixture(t)
  await preparePack(state)
  const unchanged = await preparePack(state)
  assert.equal(unchanged.copied, 0)
  assert.equal(unchanged.reused, 2)
  await writeFile(resolve(state.stageRoot, 'tools/sample/tool.exe'), 'changed program!!!')
  await assert.rejects(verifyPack(state.stageRoot), /SHA-256 verification failed/)
  const repaired = await preparePack(state)
  assert.equal(repaired.copied, 1)
  const verified = await verifyPack(state.stageRoot)
  assert.equal(verified.availableToolCount, 1)
})

test('a selective tree copies only declared application files', async t => {
  const state = await fixture(t)
  state.definition.tools[0].copies[0].include = ['tool.exe', 'NOTICE.txt']
  const result = await preparePack(state)
  assert.equal(result.copied, 2)
  assert.ok(result.excluded.some(item => item.path === 'app/cache' && item.reason.includes('selected application')))
  await verifyPack(state.stageRoot)
})

test('package paths reject traversal, drive paths, alternate streams, and mixed separators', () => {
  for (const name of ['../outside', '/absolute', 'tools/../outside', 'C:/outside', 'tools/app:stream', 'tools\\app', 'tools//app', '.']) {
    assert.throws(() => childPath(resolve(tmpdir()), name), /Invalid package path|escapes root/)
  }
})

test('source JSON cannot send entries or copies outside a registered tool root', async t => {
  const state = await fixture(t)
  state.definition.tools[0].entry.path = 'tools/other/tool.exe'
  assert.throws(() => validateDefinition(state.definition), /outside registered roots/)
  state.definition.tools[0].entry.path = 'tools/sample/tool.exe'
  state.definition.tools[0].copies[0].to = 'tools/other'
  assert.throws(() => validateDefinition(state.definition), /outside owned roots/)
})

test('required entry files must be owned and present in the prepared catalog', async t => {
  const state = await fixture(t)
  state.definition.tools[0].entry.requiredFiles = ['tools/other/engine.zip']
  assert.throws(() => validateDefinition(state.definition), /outside registered roots/)
  state.definition.tools[0].entry.requiredFiles = ['tools/sample/engine.zip']
  await assert.rejects(preparePack(state), /Packaged required entry file is missing/)
  await writeFile(resolve(state.sourceRoot, 'app/engine.zip'), 'embedded runtime')
  const result = await preparePack(state)
  assert.deepEqual(result.catalog.tools[0].entry.requiredFiles, ['tools/sample/engine.zip'])
})

test('unowned directories, overlapping source roots, and resources staging are refused', async t => {
  const state = await fixture(t)
  await mkdir(state.stageRoot)
  await writeFile(resolve(state.stageRoot, 'keep.txt'), 'unrelated')
  await assert.rejects(preparePack(state), /nonempty directory/)
  assert.equal(await readFile(resolve(state.stageRoot, 'keep.txt'), 'utf8'), 'unrelated')
  await assert.rejects(preparePack({ ...state, stageRoot: resolve(state.sourceRoot, 'stage') }), /non-overlapping/)
  await assert.rejects(preparePack({ ...state, stageRoot: resolve(state.root, 'resources/stage') }), /outside resources/)
})

test('source junctions cannot import files from outside the selected tree', async t => {
  const state = await fixture(t)
  const outside = resolve(state.root, 'outside')
  await mkdir(outside)
  await writeFile(resolve(outside, 'private.txt'), 'private')
  const link = resolve(state.sourceRoot, 'app/link')
  await symlink(outside, link, process.platform === 'win32' ? 'junction' : 'dir')
  try { await assert.rejects(planCopies(state.definition, state.sourceRoot), /Symbolic links and junctions/) }
  finally { await unlink(link) }
})

test('destination junctions are rejected before modifying their target', async t => {
  const state = await fixture(t)
  await preparePack(state)
  const outside = resolve(state.root, 'outside')
  await mkdir(outside)
  const link = resolve(state.stageRoot, 'linked')
  await symlink(outside, link, process.platform === 'win32' ? 'junction' : 'dir')
  try {
    await assert.rejects(preparePack(state), /Symbolic links and junctions/)
    await assert.rejects(verifyPack(state.stageRoot), /Symbolic links and junctions/)
  } finally { await unlink(link) }
})

test('unlisted files fail verification and are preserved for review', async t => {
  const state = await fixture(t)
  await preparePack(state)
  const extra = resolve(state.stageRoot, 'tools/sample/history.json')
  await writeFile(extra, 'user data')
  await assert.rejects(verifyPack(state.stageRoot), /Unlisted file/)
  await assert.rejects(preparePack(state), /must be reviewed/)
  assert.equal(await readFile(extra, 'utf8'), 'user data')
})

test('a missing offline download leaves local tools available and reports the missing entry', async t => {
  const state = await fixture(t)
  const download = structuredClone(state.definition.tools[0])
  download.id = 'download'
  download.roots = ['tools/download']
  download.entry = { kind: 'gui', path: 'tools/download/download.exe', cwd: 'tools/download' }
  delete download.copies
  download.downloads = [{ url: 'https://example.invalid/tool', path: 'tools/download/download.exe', sha256: '1'.repeat(64) }]
  state.definition.tools.push(download)
  const result = await preparePack(state)
  assert.equal(result.catalog.tools[0].available, true)
  assert.equal(result.catalog.tools[1].available, false)
  assert.match(result.catalog.tools[1].unavailableReason, /offline cache/)
  assert.equal(result.unavailable.length, 1)
  await verifyPack(state.stageRoot)
})

test('a pinned offline download is checked before it is included', async t => {
  const state = await fixture(t)
  const bytes = Buffer.from('downloaded program')
  const digest = createHash('sha256').update(bytes).digest('hex')
  const tool = state.definition.tools[0]
  delete tool.copies
  tool.downloads = [{ url: 'https://example.invalid/tool', path: tool.entry.path, sha256: digest }]
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `${digest}.download`), bytes)
  const result = await preparePack(state)
  assert.equal(result.catalog.tools[0].entrySha256, digest)
  await verifyPack(state.stageRoot)
})

test('a pinned source file rejects substituted bytes before copying them', async t => {
  const state = await fixture(t)
  state.definition.tools[0].copies = [{ from: 'app/tool.exe', to: 'tools/sample/tool.exe', sha256: '0'.repeat(64) }]
  await assert.rejects(preparePack(state), /Pinned source SHA-256 mismatch/)
  await assert.rejects(stat(resolve(state.stageRoot, 'tools/sample/tool.exe')), { code: 'ENOENT' })
})

test('private runtime ZIP contents are hashed, included in owned roots, and cleaned from temporary extraction', async t => {
  const state = await fixture(t)
  const bytes = zipSync({ 'dotnet.exe': Buffer.from('runtime host'), 'shared/host.dll': Buffer.from('runtime library') })
  const digest = createHash('sha512').update(bytes).digest('hex')
  state.definition.runtimes.push({ id: 'dotnet-8', archives: [{ format: 'zip', url: 'https://example.invalid/runtime.zip', sha512: digest, to: 'runtime/windows/dotnet-8' }] })
  state.definition.tools[0].roots.push('runtime/windows/dotnet-8')
  state.definition.tools[0].entry.dotnetRoot = 'runtime/windows/dotnet-8'
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `sha512-${digest}.download`), bytes)
  const result = await preparePack(state)
  assert.equal(result.catalog.tools[0].fileCount, 4)
  assert.equal(await readFile(resolve(state.stageRoot, 'runtime/windows/dotnet-8/shared/host.dll'), 'utf8'), 'runtime library')
  assert.deepEqual(await readdir(state.cacheRoot), [`sha512-${digest}.download`])
  await verifyPack(state.stageRoot)
})

test('runtime ZIP package paths cannot write outside their temporary extraction directory', async t => {
  const state = await fixture(t)
  const bytes = zipSync({ '../outside.txt': Buffer.from('invalid archive path') })
  const digest = createHash('sha512').update(bytes).digest('hex')
  state.definition.runtimes.push({ id: 'runtime', archives: [{ format: 'zip', url: 'https://example.invalid/runtime.zip', sha512: digest, to: 'runtime/windows/runtime' }] })
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `sha512-${digest}.download`), bytes)
  await assert.rejects(preparePack(state), /Invalid package path/)
  await assert.rejects(stat(resolve(state.cacheRoot, 'outside.txt')), { code: 'ENOENT' })
})

test('NSIS payload listing rejects traversal, duplicate paths, links, and ambiguous records before extraction', () => {
  const root = resolve(tmpdir(), 'native-archive-listing')
  assert.deepEqual(validateArchiveListing('Path = bins\\yak.zip\nSize = 20\n\nPath = Yakit.exe\nSize = 40\n', root), ['bins/yak.zip', 'yakit.exe'])
  for (const listing of [
    'Path = ..\\outside.exe\nSize = 4',
    'Path = file.exe\n\nPath = FILE.exe',
    'Path = link\nSymbolic Link = outside',
    'Path = link\nHard Link = outside',
    'Path = link\nAttributes = A_ lrwxrwxrwx',
    'Path = first\nPath = second',
    '',
  ]) assert.throws(() => validateArchiveListing(listing, root), /Invalid package path|Duplicate native archive|Unsupported native archive/)
})

test('tool archive definitions require a hash and owned extraction root', async t => {
  const state = await fixture(t)
  const archive = { format: 'nsis-7z', url: 'https://example.invalid/tool.exe', to: 'tools/sample', sha256: '1'.repeat(64) }
  state.definition.tools[0].archives = [archive]
  validateDefinition(state.definition)
  archive.to = 'tools/other'
  assert.throws(() => validateDefinition(state.definition), /Unpinned native tool archive/)
  archive.to = 'tools/sample'
  archive.sha256 = 'not-pinned'
  assert.throws(() => validateDefinition(state.definition), /Unpinned native tool archive/)
  archive.sha256 = '1'.repeat(64)
  archive.requiredFiles = [{ path: '../outside', sha256: '1'.repeat(64) }]
  assert.throws(() => validateDefinition(state.definition), /Invalid package path/)
})

test('NSIS preparation requires an explicit archive extractor before acquiring a payload', async t => {
  const state = await fixture(t)
  state.definition.tools[0].archives = [{ format: 'nsis-7z', url: 'https://example.invalid/tool.exe', to: 'tools/sample', sha256: '1'.repeat(64) }]
  await assert.rejects(preparePack(state), /Pass --seven-zip/)
  await assert.rejects(stat(state.cacheRoot), { code: 'ENOENT' })
})

test('a pinned tool ZIP selects a distribution subdirectory and retains its required files', async t => {
  const state = await fixture(t)
  const executable = Buffer.from('packaged executable')
  const executableHash = createHash('sha256').update(executable).digest('hex')
  const bytes = zipSync({ 'release/tool.exe': executable, 'release/NOTICE.txt': Buffer.from('upstream license'), 'release/unrelated/data.txt': Buffer.from('unselected'), 'other/build.txt': Buffer.from('unselected') })
  const digest = createHash('sha256').update(bytes).digest('hex')
  const tool = state.definition.tools[0]
  delete tool.copies
  tool.archives = [{ format: 'zip', url: 'https://example.invalid/tool.zip', to: 'tools/sample', from: 'release', include: ['tool.exe', 'NOTICE.txt'], sha256: digest, requiredFiles: [{ path: 'tool.exe', sha256: executableHash }] }]
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `${digest}.download`), bytes)
  const result = await preparePack(state)
  assert.equal(result.catalog.tools[0].entrySha256, executableHash)
  assert.equal(result.catalog.tools[0].archives, undefined)
  assert.equal(result.catalog.tools[0].fileCount, 2)
  assert.ok(result.excluded.some(item => item.path === 'application/release/unrelated'))
  assert.deepEqual(await readdir(state.cacheRoot), [`${digest}.download`])
  await verifyPack(state.stageRoot)
})

test('a substituted required ZIP file stops before copying application files', async t => {
  const state = await fixture(t)
  const bytes = zipSync({ 'tool.exe': Buffer.from('substituted executable') })
  const digest = createHash('sha256').update(bytes).digest('hex')
  delete state.definition.tools[0].copies
  state.definition.tools[0].archives = [{ format: 'zip', url: 'https://example.invalid/tool.zip', to: 'tools/sample', sha256: digest, requiredFiles: [{ path: 'tool.exe', sha256: '0'.repeat(64) }] }]
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `${digest}.download`), bytes)
  await assert.rejects(preparePack(state), /Required archive file SHA-256 mismatch/)
  await assert.rejects(stat(resolve(state.stageRoot, 'tools/sample/tool.exe')), { code: 'ENOENT' })
  assert.deepEqual(await readdir(state.cacheRoot), [`${digest}.download`])
})

test('a pinned runtime ZIP selects its release directory without requiring a system installation', async t => {
  const state = await fixture(t)
  const bytes = zipSync({ 'jre-release/bin/javaw.exe': Buffer.from('private java'), 'unrelated.txt': Buffer.from('other') })
  const digest = createHash('sha256').update(bytes).digest('hex')
  state.definition.runtimes = [{ id: 'java', archives: [{ format: 'zip', url: 'https://example.invalid/java.zip', sha256: digest,
    from: 'jre-release', to: 'runtime/windows/java' }] }]
  state.definition.tools[0].roots.push('runtime/windows/java')
  state.definition.tools[0].entry.runtime = 'runtime/windows/java/bin/javaw.exe'
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `${digest}.download`), bytes)
  await preparePack(state)
  assert.equal(await readFile(resolve(state.stageRoot, state.definition.tools[0].entry.runtime), 'utf8'), 'private java')
  await assert.rejects(stat(resolve(state.stageRoot, 'runtime/windows/java/unrelated.txt')), { code: 'ENOENT' })
  state.definition.runtimes[0].archives.push({ ...state.definition.runtimes[0].archives[0], from: 'absent-release', to: 'runtime/windows/java-extra' })
  await assert.rejects(preparePack(state), /Runtime archive directory is missing/)
  state.definition.runtimes[0].archives.pop()
  state.definition.runtimes[0].archives[0].from = 'absent-release'
  await assert.rejects(preparePack(state), /Runtime archive directory is missing/)
  state.definition.runtimes[0].archives[0].from = '../outside'
  assert.throws(() => validateDefinition(state.definition), /Invalid package path/)
  state.definition.runtimes[0].archives[0].from = 'jre-release'
  state.definition.runtimes[0].archives[0].sha512 = '0'.repeat(128)
  assert.throws(() => validateDefinition(state.definition), /Unpinned runtime archive/)
})

test('a pinned gzip tar can supply a private Python runtime and application-local DLLs', async t => {
  const state = await fixture(t)
  const tree = resolve(state.root, 'python-archive')
  await mkdir(resolve(tree, 'python'), { recursive: true })
  for (const name of ['python.exe', 'python3.dll', 'python312.dll']) await writeFile(resolve(tree, 'python', name), name)
  const archive = resolve(state.root, 'python.tar.gz')
  await createTar({ file: archive, cwd: tree, gzip: true }, ['python'])
  const bytes = await readFile(archive)
  const digest = createHash('sha256').update(bytes).digest('hex')
  const tool = state.definition.tools[0]
  tool.entry.pythonRoot = 'tools/sample/python312'
  tool.entry.requiredFiles = ['tools/sample/python312/python312.dll', 'tools/sample/python3.dll']
  tool.archives = [
    { format: 'tar.gz', url: 'https://example.invalid/python.tar.gz', sha256: digest, from: 'python', to: tool.entry.pythonRoot },
    { format: 'tar.gz', url: 'https://example.invalid/python.tar.gz', sha256: digest, from: 'python', to: 'tools/sample', include: ['python3.dll', 'python312.dll'] },
  ]
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `${digest}.download`), bytes)
  await preparePack(state)
  await verifyPack(state.stageRoot)
  assert.equal(await readFile(resolve(state.stageRoot, 'tools/sample/python3.dll'), 'utf8'), 'python3.dll')
  assert.equal(await readFile(resolve(state.stageRoot, 'tools/sample/python312/python.exe'), 'utf8'), 'python.exe')
  assert.deepEqual(await readdir(state.cacheRoot), [`${digest}.download`])
})

test('a gzip tar with a parent directory member is rejected before tool files are copied', async t => {
  const state = await fixture(t)
  const archive = resolve(state.root, 'invalid.tar.gz')
  await createTar({ file: archive, cwd: state.sourceRoot, gzip: true, prefix: '../outside' }, ['app/tool.exe'])
  const bytes = await readFile(archive)
  const digest = createHash('sha256').update(bytes).digest('hex')
  state.definition.tools[0].archives = [{ format: 'tar.gz', url: 'https://example.invalid/invalid.tar.gz', sha256: digest, to: 'tools/sample' }]
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `${digest}.download`), bytes)
  await assert.rejects(preparePack(state), /Unsafe release path/)
  await assert.rejects(stat(resolve(state.stageRoot, 'tools/sample/tool.exe')), { code: 'ENOENT' })
  assert.deepEqual(await readdir(state.cacheRoot), [`${digest}.download`])
})

test('native ZIP traversal entries are rejected and temporary files are removed', async t => {
  const state = await fixture(t)
  const bytes = zipSync({ '../outside.txt': Buffer.from('invalid archive path') })
  const digest = createHash('sha256').update(bytes).digest('hex')
  state.definition.tools[0].archives = [{ format: 'zip', url: 'https://example.invalid/tool.zip', to: 'tools/sample', sha256: digest }]
  await mkdir(state.cacheRoot)
  await writeFile(resolve(state.cacheRoot, `${digest}.download`), bytes)
  await assert.rejects(preparePack(state), /Invalid package path/)
  assert.deepEqual(await readdir(state.cacheRoot), [`${digest}.download`])
})

test('generated configuration files are hashed with the tool inventory without changing source files', async t => {
  const state = await fixture(t)
  const content = 'interpreter=./bin/pythonw.exe\n'
  state.definition.tools[0].generatedFiles = [{ path: 'tools/sample/config/interpreters.ini', content }]
  const result = await preparePack(state)
  const inventory = await verifyPack(state.stageRoot)
  assert.equal(await readFile(resolve(state.stageRoot, 'tools/sample/config/interpreters.ini'), 'utf8'), content)
  assert.equal(inventory.files.find(file => file.path === 'tools/sample/config/interpreters.ini').sha256, createHash('sha256').update(content).digest('hex'))
  assert.equal(result.catalog.tools[0].generatedFiles, undefined)
  assert.deepEqual(await readdir(state.cacheRoot), [])
  assert.equal(await readFile(resolve(state.sourceRoot, 'app/tool.exe'), 'utf8'), 'test program bytes')
  await assert.rejects(stat(resolve(state.sourceRoot, 'app/config/interpreters.ini')), { code: 'ENOENT' })
})

test('generated file definitions reject paths outside their tool and non-text contents', async t => {
  const state = await fixture(t)
  for (const file of [
    { path: '../outside', content: 'text' },
    { path: 'tools/other/config.ini', content: 'text' },
    { path: 'runtime/windows/runtime/config.ini', content: 'text' },
    { path: 'tools/sample/config.ini', content: null },
    { path: 'tools/sample/config.ini', content: 'text\0' },
  ]) {
    state.definition.tools[0].generatedFiles = [file]
    assert.throws(() => validateDefinition(state.definition), /Invalid package path|Invalid generated tool file/)
  }
})

for (const source of ['copy', 'archive', 'download', 'generated']) {
  test(`generated files cannot replace a ${source} destination`, async t => {
    const state = await fixture(t)
    const tool = state.definition.tools[0]
    tool.generatedFiles = [{ path: tool.entry.path, content: 'replacement' }]
    if (source !== 'copy') delete tool.copies
    if (source === 'generated') tool.generatedFiles.push({ path: tool.entry.path.toUpperCase().replace('TOOLS/SAMPLE/', 'tools/sample/'), content: 'duplicate' })
    if (source === 'archive' || source === 'download') {
      const bytes = source === 'archive' ? zipSync({ 'tool.exe': Buffer.from('archived program') }) : Buffer.from('downloaded program')
      const digest = createHash('sha256').update(bytes).digest('hex')
      await mkdir(state.cacheRoot)
      await writeFile(resolve(state.cacheRoot, `${digest}.download`), bytes)
      if (source === 'archive') tool.archives = [{ format: 'zip', url: 'https://example.invalid/tool.zip', to: 'tools/sample', sha256: digest }]
      else tool.downloads = [{ url: 'https://example.invalid/tool.exe', path: tool.entry.path, sha256: digest }]
    }
    await assert.rejects(preparePack(state), /Duplicate package destination/)
    await assert.rejects(stat(resolve(state.stageRoot, tool.entry.path)), { code: 'ENOENT' })
  })
}

test('the shipped definition has thirty-eight tools and a packaged x32dbg variant', async () => {
  const definition = validateDefinition(JSON.parse(await readFile(new URL('../toolpacks/native-tools.sources.json', import.meta.url), 'utf8')))
  assert.equal(definition.tools.length, 38)
  assert.deepEqual(definition.tools.filter(tool => tool.category === 'web').map(tool => tool.id), ['yakit', 'cyberchef', 'curl', 'jq', 'yq', 'bruno'])
  assert.equal(definition.tools.filter(tool => tool.category === 'misc').length, 25)
  assert.equal(definition.tools.some(tool => tool.id === 'burp-community'), false)
  const yakit = definition.tools.find(tool => tool.id === 'yakit')
  assert.equal(yakit.entry.kind, 'gui')
  assert.equal(yakit.entry.path, 'tools/yakit/Yakit.exe')
  assert.equal(yakit.archives[0].format, 'nsis-7z')
  assert.ok(yakit.archives[0].requiredFiles.some(file => file.path === 'bins/yak.zip'))
  assert.ok(yakit.preserve.includes('tools/yakit/yakit-projects'))
  assert.deepEqual(definition.tools.find(tool => tool.id === 'x64dbg').variants.map(variant => variant.id), ['x32'])
  const ida = definition.tools.find(tool => tool.id === 'ida')
  assert.equal(ida.entry.pythonRoot, 'tools/ida/python312')
  assert.ok(ida.copies[0].exclude.includes('python38'))
  assert.ok(ida.entry.requiredFiles.includes('tools/ida/python312/Lib/site-packages/PyQt5/sip.cp312-win_amd64.pyd'))
  assert.ok(ida.preserve.includes('tools/ida/ida.key'))
  assert.equal(definition.tools.find(tool => tool.id === 'imhex').entry.dotnetRoot, 'runtime/windows/dotnet-8')
  assert.equal(ida.copies[0].exclude.includes('ida.key'), false)
  for (const name of ['Keygen.7z', 'IDA_InitTool.exe']) assert.ok(ida.copies[0].exclude.includes(name))
})
