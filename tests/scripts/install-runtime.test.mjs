/** The WSL runtime installer: unpacking this version, and removing what earlier versions left. Linux only (flock). */
import assert from 'node:assert/strict'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const script = fileURLToPath(new URL('../../scripts/install-runtime.py', import.meta.url))
const linux = process.platform === 'linux'
const digestOf = text => createHash('sha256').update(text).digest('hex')

function workspace(t) {
  const home = mkdtempSync(join(tmpdir(), 'rainy-runtime-home-'))
  t.after(() => rmSync(home, { recursive: true, force: true }))
  return { home, owned: join(home, '.rainy-agent/runtime') }
}

const python = (home, args) => JSON.parse(execFileSync('python3', [script, ...args], { env: { ...process.env, HOME: home }, encoding: 'utf8' }))

test('unpacks this version and asks for the archive when an earlier one is left', { skip: !linux }, (t) => {
  const { home, owned } = workspace(t)
  const source = join(home, 'source')
  mkdirSync(join(source, 'node/bin'), { recursive: true })
  mkdirSync(join(source, 'app/dist'), { recursive: true })
  writeFileSync(join(source, 'node/bin/node'), 'node')
  writeFileSync(join(source, 'app/dist/host.js'), 'host')
  const archive = join(home, 'linux-runtime.tar.gz')
  execFileSync('tar', ['-czf', archive, '-C', source, 'node', 'app'])
  const bytes = readFileSync(archive)
  const metadata = join(home, 'linux-runtime.json')
  writeFileSync(metadata, JSON.stringify({ sha256: digestOf(bytes), bytes: bytes.length }))

  const stale = join(home, 'stale.tar.gz')
  writeFileSync(stale, 'an archive from an earlier version')
  assert.deepEqual(python(home, [stale, metadata]), { needsArchive: true })
  const installed = python(home, [archive, metadata])
  assert.equal(installed.host, join(owned, digestOf(bytes), 'app/dist/host.js'))
  assert.equal(readFileSync(installed.node, 'utf8'), 'node')
})

test('removes earlier runtimes and abandoned staging, keeping the current, running, incomplete and linked ones', { skip: !linux }, async (t) => {
  const { home, owned } = workspace(t)
  const runtime = (name, complete = true) => {
    mkdirSync(join(owned, name, 'node/bin'), { recursive: true })
    if (complete) writeFileSync(join(owned, name, '.complete'), `${name}\n`)
    return join(owned, name)
  }
  const [current, earlier, running, incomplete, linked] = ['current', 'earlier', 'running', 'incomplete', 'linked'].map(digestOf)
  runtime(current)
  runtime(earlier)
  const busy = runtime(running)
  runtime(incomplete, false)
  mkdirSync(join(owned, '.install-abandoned'))
  const outside = mkdtempSync(join(tmpdir(), 'rainy-runtime-outside-'))
  t.after(() => rmSync(outside, { recursive: true, force: true }))
  writeFileSync(join(outside, '.complete'), 'outside\n')
  symlinkSync(outside, join(owned, linked))
  const metadata = join(home, 'linux-runtime.json')
  writeFileSync(metadata, JSON.stringify({ sha256: current, bytes: 1 }))
  const sleeper = spawn('sleep', ['30'], { cwd: busy, stdio: 'ignore' })
  t.after(() => sleeper.kill())
  await new Promise(resolve => sleeper.once('spawn', resolve))

  const { removed } = python(home, ['--prune', metadata])
  assert.deepEqual(removed.sort(), [earlier, '.install-abandoned'].sort())
  for (const name of [current, running, incomplete, linked]) assert.ok(existsSync(join(owned, name)), name)
  assert.ok(!existsSync(join(owned, earlier)))
  assert.ok(existsSync(join(outside, '.complete')))
  assert.deepEqual(execFileSync('ls', ['-A', owned], { encoding: 'utf8' }).split('\n').filter(name => name.startsWith('.trash-')), [])
  assert.deepEqual(python(home, ['--prune', metadata]), { removed: [] })
})
