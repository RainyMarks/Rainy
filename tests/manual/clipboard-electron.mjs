/** Build an isolated Electron fixture for conversation text copying. */
import { createRequire } from 'node:module'
import { mkdtemp, writeFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { spawn } from 'node:child_process'
import assert from 'node:assert/strict'
const require = createRequire(new URL('../../package.json', import.meta.url))
const { build } = require('esbuild')
const root = await mkdtemp(join(tmpdir(), 'rainy-clipboard-fixture-'))
try {
  await writeFile(join(root, 'package.json'), JSON.stringify({ main: 'main.cjs' }))
  await build({ entryPoints: [new URL('./fixtures/clipboard-electron.ts', import.meta.url).pathname.replace(/^\/(?=[A-Za-z]:)/, '')], outfile: join(root, 'main.cjs'), bundle: true,
    platform: 'node', format: 'cjs', external: ['electron'] })
  const child = spawn(require('electron'), [root], { windowsHide: true, stdio: 'inherit' })
  const timeout = setTimeout(() => child.kill(), 30000)
  const code = await new Promise(resolve => child.once('exit', resolve))
  clearTimeout(timeout)
  assert.equal(code, 0)
} finally { await rm(root, { recursive: true, force: true }) }
