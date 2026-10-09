/**
 * Run the Host without Electron for development and headless checks.
 *
 * `pnpm run dev [--no-build] [--home DIR] [--port N]` builds the Host and renderer, starts `node dist/host.js` with a
 * private `RAINY_HOME` (default `<repo>/tmp/dev-home`), prints the workbench URL from the Host's `ready` control line,
 * and turns Ctrl+C into a `{"type":"stop"}` control line. A second Ctrl+C kills the Host.
 */
import { spawn } from 'node:child_process'
import { mkdirSync } from 'node:fs'
import { resolve } from 'node:path'
import { createInterface } from 'node:readline'
import { parseArgs } from 'node:util'
import { parseReady } from '../src/main/transport.ts'

const root = resolve(import.meta.dirname, '..')
const { values } = parseArgs({ options: { 'no-build': { type: 'boolean', default: false }, home: { type: 'string' }, port: { type: 'string' } } })
const home = resolve(values.home ?? resolve(root, 'tmp/dev-home'))
if (values.port !== undefined && !/^\d{1,5}$/u.test(values.port)) throw new Error('--port must be a TCP port number')

if (!values['no-build']) {
  const code = await new Promise<number | null>((accept, reject) => {
    const build = spawn(process.execPath, ['--import', 'tsx', resolve(root, 'scripts/build.ts')], { cwd: root, stdio: 'inherit' })
    build.once('error', reject)
    build.once('exit', accept)
  })
  if (code !== 0) process.exit(code ?? 1)
}

mkdirSync(home, { recursive: true, mode: 0o700 })
const env: NodeJS.ProcessEnv = { ...process.env, RAINY_HOME: home, RAINY_DETACHED: '1' }
if (values.port !== undefined) env.RAINY_PORT = values.port
// A separate process group keeps the terminal's Ctrl+C away from the Host, which stops through its control line instead.
const host = spawn(process.execPath, [resolve(root, 'dist/host.js')], { cwd: root, env, stdio: ['pipe', 'pipe', 'inherit'],
  detached: process.platform !== 'win32' })
const lines = createInterface({ input: host.stdout, crlfDelay: Infinity })
lines.on('line', (line) => {
  if (!line.startsWith('RAINY_CONTROL ')) { process.stdout.write(line + '\n'); return }
  let message: unknown
  try { message = JSON.parse(line.slice('RAINY_CONTROL '.length)) }
  catch (_malformed) { console.error(`Unreadable control line: ${line}`); return } // The Host keeps running; the line is shown for diagnosis.
  if (message === null || typeof message !== 'object' || !('type' in message)) return
  if (message.type === 'ready') {
    const ready = parseReady(message)
    console.log(`RainyAgent Host ${ready.pid} is ready: ${ready.url}\nHome: ${ready.home}\nPress Ctrl+C to stop.`)
  } else if (message.type === 'fatal') {
    console.error(`Host startup failed: ${'message' in message ? String(message.message) : 'unknown error'}`)
  }
})

let stopping = false
process.on('SIGINT', () => {
  if (stopping) { host.kill('SIGKILL'); return }
  stopping = true
  console.log('Stopping the Host; press Ctrl+C again to kill it.')
  host.stdin.end(JSON.stringify({ type: 'stop' }) + '\n')
})
host.once('error', (error) => { console.error(error.message); process.exitCode = 1 })
host.once('exit', (code, signal) => {
  if (signal !== null && !stopping) console.error(`Host stopped by ${signal}`)
  process.exit(code ?? (stopping ? 0 : 1))
})
