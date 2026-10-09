/** Exercise the actual Windows-to-WSL control transport against an installed runtime. */
import { readFile } from 'node:fs/promises'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { randomUUID } from 'node:crypto'
import assert from 'node:assert/strict'
import { WslHostTransport } from '../src/transport.ts'

const runtime = JSON.parse(await readFile(process.argv[2], 'utf8')) as { node: string; host: string }
process.env.RAINY_HOME = '/tmp/rainy-lifecycle-' + randomUUID()
process.env.RAINY_PORT = '0'
process.env.WSLENV = [process.env.WSLENV, 'RAINY_HOME', 'RAINY_PORT'].filter(Boolean).join(':')
const transport = new WslHostTransport({ distro: 'Ubuntu', node: runtime.node, entry: runtime.host })
const started = performance.now()
const ready = await transport.start()
const startupMs = performance.now() - started
try {
  assert.equal(ready.home, process.env.RAINY_HOME)
  assert.equal((await fetch(ready.url, { redirect: 'manual' })).status, 303)
} finally { await transport.stop() }
const check = await promisify(execFile)('wsl.exe', ['-d', 'Ubuntu', '--exec', 'python3', '-c', 'import os,sys; print(os.path.exists("/proc/"+sys.argv[1]))', String(ready.pid)], { windowsHide: true })
assert.equal(check.stdout.trim(), 'False', 'Control shutdown must leave no Linux Host behind')
console.log(JSON.stringify({ kind: 'windows-wsl-stdio-lifecycle', startupMs, controlProtocol: ready.protocol, orderlyShutdown: 'passed', orphanedHost: false }))
