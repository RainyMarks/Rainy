/** The Linux runtime installer: a downloaded base and a shipped application layer, reuse of an unchanged base across
 *  versions and from a runtime unpacked before the layers, pruning, and the SSH modes. Linux only (flock, symlinks). */
import assert from 'node:assert/strict'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const script = fileURLToPath(new URL('../../scripts/install-runtime.py', import.meta.url))
const linux = process.platform === 'linux'
const digestOf = bytes => createHash('sha256').update(bytes).digest('hex')

function workspace(t) {
  const home = mkdtempSync(join(tmpdir(), 'rainy-runtime-home-'))
  t.after(() => rmSync(home, { recursive: true, force: true }))
  return { home, owned: join(home, '.rainy-agent/runtime') }
}

const python = (home, args, input) => JSON.parse(execFileSync('python3', [script, ...args], { env: { ...process.env, HOME: home }, input, encoding: 'utf8' }))
/** The installer's own content identity of a directory. */
const contentOf = directory => execFileSync('python3', ['-B', '-c', `import importlib.util,sys
spec = importlib.util.spec_from_file_location('r', sys.argv[1]); m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
print(m.content_digest(sys.argv[2]))`, script, directory], { encoding: 'utf8' }).trim()

/** A full runtime tree as stage-linux.py stages it, with `host` as the Host's text. */
function stage(home, name, host, node = 'node 22') {
  const root = join(home, name)
  for (const directory of ['node/bin', 'packages/ws', 'app/dist/renderer', 'app/resources/ide/bin', 'app/resources/pdfjs', 'app/bin']) {
    mkdirSync(join(root, directory), { recursive: true })
  }
  writeFileSync(join(root, 'node/bin/node'), node, { mode: 0o755 })
  writeFileSync(join(root, 'packages/ws/index.js'), 'ws')
  writeFileSync(join(root, 'app/resources/ide/bin/ruff'), 'ruff', { mode: 0o755 })
  writeFileSync(join(root, 'app/resources/pdfjs/pdf.min.js'), 'pdf')
  writeFileSync(join(root, 'app/dist/host.js'), host)
  writeFileSync(join(root, 'app/dist/renderer/index.html'), 'page')
  writeFileSync(join(root, 'app/package.json'), '{}')
  writeFileSync(join(root, 'runtime.json'), JSON.stringify({ version: host }))
  mkdirSync(join(root, 'app/node_modules'))
  symlinkSync('../../packages/ws', join(root, 'app/node_modules/ws'))
  symlinkSync('../../packages/ws/index.js', join(root, 'app/bin/rg'))
  return root
}

/** Archives and metadata for one version, split by the installer's rules; the base is xz as stage-linux.py packs it. */
function release(home, root, compression = 'xz') {
  const base = `${root}-base.tar.${compression}`
  const app = `${root}-app.tar.gz`
  execFileSync('tar', [compression === 'xz' ? '-cJf' : '-czf', base, '-C', root, 'node', 'packages', 'app/node_modules', 'app/bin', 'app/resources/ide'])
  execFileSync('tar', ['-czf', app, '-C', root, 'runtime.json', 'app/dist', 'app/package.json', 'app/resources/pdfjs'])
  const baseBytes = readFileSync(base)
  const appBytes = readFileSync(app)
  const metadata = { sha256: digestOf(baseBytes), bytes: baseBytes.length, content: contentOf(root), app: { sha256: digestOf(appBytes), bytes: appBytes.length } }
  return { base, app, baseBytes, appBytes, metadata, metadataPath: (() => { const path = `${root}.json`; writeFileSync(path, JSON.stringify(metadata)); return path })() }
}

test('installs the base once and only the application layer for a version whose base did not change', { skip: !linux }, (t) => {
  const { home, owned } = workspace(t)
  const first = release(home, stage(home, 'v1', 'host 1'))
  const missing = join(home, 'not-downloaded.tar.gz')
  assert.deepEqual(python(home, [missing, first.app, first.metadataPath]), { needsArchive: true })
  const one = python(home, [first.base, first.app, first.metadataPath])
  assert.equal(one.host, join(owned, first.metadata.app.sha256, 'app/dist/host.js'))
  assert.equal(readFileSync(one.node, 'utf8'), 'node 22')

  const second = release(home, stage(home, 'v2', 'host 2'))
  assert.equal(second.metadata.content, first.metadata.content)
  const two = python(home, [missing, second.app, second.metadataPath])
  assert.equal(readFileSync(two.host, 'utf8'), 'host 2')
  assert.equal(readFileSync(two.node, 'utf8'), 'node 22')
  const runtime = join(owned, second.metadata.app.sha256)
  assert.equal(readFileSync(join(runtime, 'app/node_modules/ws/index.js'), 'utf8'), 'ws')
  assert.equal(readFileSync(join(runtime, 'app/resources/ide/bin/ruff'), 'utf8'), 'ruff')
  assert.equal(readFileSync(join(runtime, 'app/resources/pdfjs/pdf.min.js'), 'utf8'), 'pdf')
  assert.ok(lstatSync(join(runtime, 'node')).isSymbolicLink())
  assert.deepEqual(readdirSync(owned).filter(name => name.startsWith('base-')), [`base-${first.metadata.content}`])

  // A changed base, such as a new Node.js, is downloaded again.
  const third = release(home, stage(home, 'v3', 'host 3', 'node 24'))
  assert.notEqual(third.metadata.content, first.metadata.content)
  assert.deepEqual(python(home, [missing, third.app, third.metadataPath]), { needsArchive: true })
  assert.throws(() => python(home, [third.base, second.app, third.metadataPath]), /application files do not match/u)
})

test('unpacks a gzip base, and an xz base with a Python that has no lzma module', { skip: !linux }, (t) => {
  const { home, owned } = workspace(t)
  const gzip = release(home, stage(home, 'gz', 'host gz'), 'gz')
  assert.equal(readFileSync(python(home, [gzip.base, gzip.app, gzip.metadataPath]).host, 'utf8'), 'host gz')
  const xz = release(home, stage(home, 'xz', 'host xz', 'node 26'))
  // A Python built without liblzma: the installer hands the archive to the system's xz.
  const noLzma = `import runpy, sys\nsys.modules['lzma'] = None\nsys.argv = sys.argv[1:]\nrunpy.run_path(sys.argv[0], run_name='__main__')`
  const installed = JSON.parse(execFileSync('python3', ['-c', noLzma, script, xz.base, xz.app, xz.metadataPath], { env: { ...process.env, HOME: home }, encoding: 'utf8' }))
  assert.equal(readFileSync(installed.node, 'utf8'), 'node 26')
  assert.deepEqual(readdirSync(owned).filter(name => name.startsWith('.install-')), [])
})

test('reuses the base of a runtime unpacked before the layers, by hard links, when its content is the same', { skip: !linux }, (t) => {
  const { home, owned } = workspace(t)
  const old = stage(home, 'old', 'host 0')
  mkdirSync(owned, { recursive: true })
  const earlier = join(owned, digestOf('3.5.1 full runtime'))
  execFileSync('cp', ['-a', old, earlier])
  writeFileSync(join(earlier, '.complete'), 'earlier\n')
  const next = release(home, stage(home, 'next', 'host 1'))
  const installed = python(home, [join(home, 'not-downloaded.tar.gz'), next.app, next.metadataPath])
  assert.equal(readFileSync(installed.host, 'utf8'), 'host 1')
  const base = join(owned, `base-${next.metadata.content}`)
  assert.equal(statSync(join(base, 'node/bin/node')).ino, statSync(join(earlier, 'node/bin/node')).ino)
  assert.equal(readFileSync(join(base, 'app/resources/ide/bin/ruff'), 'utf8'), 'ruff')
  assert.ok(!existsSync(join(base, 'app/dist')))
  assert.ok(!existsSync(join(base, 'runtime.json')))
  assert.equal(contentOf(base), next.metadata.content)
})

test('removes earlier runtimes and unused bases, keeping the current, running, incomplete and linked ones', { skip: !linux }, async (t) => {
  const { home, owned } = workspace(t)
  const first = release(home, stage(home, 'v1', 'host 1'))
  const one = python(home, [first.base, first.app, first.metadataPath])
  const second = release(home, stage(home, 'v2', 'host 2', 'node 24'))
  python(home, [second.base, second.app, second.metadataPath])
  const third = release(home, stage(home, 'v3', 'host 3', 'node 24'))
  python(home, [join(home, 'none'), third.app, third.metadataPath])
  const old = join(owned, digestOf('full runtime'))
  mkdirSync(join(old, 'node/bin'), { recursive: true })
  writeFileSync(join(old, '.complete'), 'old\n')
  const incomplete = join(owned, digestOf('incomplete'))
  mkdirSync(incomplete)
  mkdirSync(join(owned, '.install-abandoned'))
  const outside = mkdtempSync(join(tmpdir(), 'rainy-runtime-outside-'))
  t.after(() => rmSync(outside, { recursive: true, force: true }))
  writeFileSync(join(outside, '.complete'), 'outside\n')
  symlinkSync(outside, join(owned, digestOf('linked')))
  // Version 1 is still running, from its base's Node.js and its own Host.
  const sleeper = spawn('sleep', ['30'], { cwd: join(owned, first.metadata.app.sha256), stdio: 'ignore' })
  t.after(() => sleeper.kill())
  await new Promise(resolve => sleeper.once('spawn', resolve))

  const { removed } = python(home, ['--prune', third.metadataPath])
  assert.deepEqual(removed.sort(), [second.metadata.app.sha256, old.split('/').at(-1), '.install-abandoned'].sort())
  for (const kept of [third.metadata.app.sha256, `base-${third.metadata.content}`, first.metadata.app.sha256, `base-${first.metadata.content}`]) {
    assert.ok(existsSync(join(owned, kept)), kept)
  }
  assert.ok(existsSync(incomplete))
  assert.ok(existsSync(join(outside, '.complete')))
  assert.equal(readFileSync(one.node, 'utf8'), 'node 22')
  assert.deepEqual(python(home, ['--prune', third.metadataPath]), { removed: [] })
})

test('an SSH server takes the application layer on standard input, then the base sent after it or downloaded there', { skip: !linux }, async (t) => {
  const { home, owned } = workspace(t)
  const version = release(home, stage(home, 'v1', 'host 1'))
  const metadata = JSON.stringify(version.metadata)
  assert.deepEqual(python(home, ['--fetch', metadata, '[]'], version.appBytes), { needsArchive: true })
  assert.deepEqual(python(home, ['--receive', metadata], Buffer.concat([version.appBytes, version.baseBytes.subarray(0, 100)])), { needsArchive: true })
  const installed = python(home, ['--receive', metadata], Buffer.concat([version.appBytes, version.baseBytes]))
  assert.equal(installed.host, join(owned, version.metadata.app.sha256, 'app/dist/host.js'))
  assert.deepEqual(python(home, ['--fetch', metadata, '[]'], version.appBytes), installed)

  // The next version with the same base downloads nothing; one with a new base downloads it in verified pieces.
  const next = release(home, stage(home, 'v2', 'host 2'))
  assert.equal(readFileSync(python(home, ['--fetch', JSON.stringify(next.metadata), '[]'], next.appBytes).host, 'utf8'), 'host 2')
  const changed = release(home, stage(home, 'v3', 'host 3', 'node 24'))
  const parts = [changed.baseBytes.subarray(0, 50), changed.baseBytes.subarray(50)]
  const { createServer } = await import('node:http')
  const server = createServer((request, response) => { response.end(parts[Number(request.url.slice(1))]) })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  t.after(() => server.close())
  const url = `http://127.0.0.1:${server.address().port}/`
  const pieces = JSON.stringify(parts.map((part, index) => ({ url: url + index, bytes: part.length, sha256: digestOf(part) })))
  const child = spawn('python3', [script, '--fetch', JSON.stringify(changed.metadata), pieces], { env: { ...process.env, HOME: home, NO_PROXY: '*' } })
  let stdout = ''
  let stderr = ''
  child.stdout.on('data', data => { stdout += data })
  child.stderr.on('data', data => { stderr += data })
  child.stdin.end(changed.appBytes)
  await new Promise(resolve => child.once('close', resolve))
  assert.equal(readFileSync(JSON.parse(stdout).node, 'utf8'), 'node 24')
  assert.match(stderr, new RegExp(`RAINY_PROGRESS ${changed.baseBytes.length} ${changed.baseBytes.length}`))
  assert.deepEqual(readdirSync(owned).filter(name => name.startsWith('.download-')), [])
})
