/** Private real-profile browser harness; all data and model fixtures belong to the test run. */
import { spawn } from 'node:child_process'
import { createInterface } from 'node:readline'
import { createRequire } from 'node:module'
import { resolve } from 'node:path'

const requireWeb = createRequire(resolve('apps/web/package.json'))
const { chromium } = requireWeb('playwright')

/** Start a packaged Rainy profile and authenticate an isolated browser to it. */
export async function openWorkbenchHarness({ runtime, home, fixture = false, assistantCode = false, viewport = { width: 1380, height: 920 }, distro = 'Ubuntu', user }) {
  if (assistantCode && !fixture) throw new Error('assistantCode requires the private fixture profile')
  const privateHome = home ?? `/tmp/rainy-icesky-browser-${Date.now()}-${process.pid}`
  const child = spawn('wsl.exe', ['-d', distro, ...(user ? ['-u', user] : []), '--exec', 'env', `RAINY_HOME=${privateHome}`, 'RAINY_CONFIGURE_DEEPSEEK=0',
    ...(assistantCode ? ['RAINY_IDE_CODE_FIXTURE=1'] : []),
    `${runtime}/node/bin/node`, '--expose-internals', `${runtime}/app/lib/${fixture ? 'icesky-profile-test.js' : 'host.js'}`],
  { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] })
  const exited = new Promise(resolveExit => child.once('exit', resolveExit))
  let diagnostics = ''
  child.stderr.on('data', chunk => { diagnostics = (diagnostics + String(chunk)).slice(-6000) })
  const lines = createInterface({ input: child.stdout })
  let browser
  let context
  async function stop() {
    await context?.close()
    await browser?.close()
    lines.close()
    if (child.stdin.writable) child.stdin.end('{"type":"stop"}\n')
    const timeout = setTimeout(() => child.kill(), 10000)
    await exited
    clearTimeout(timeout)
  }
  try {
    const ready = await new Promise((resolveReady, reject) => {
      const deadline = setTimeout(() => reject(new Error(`Host readiness timed out: ${diagnostics}`)), 60000)
      child.once('error', error => { clearTimeout(deadline); reject(error) })
      child.once('exit', code => { clearTimeout(deadline); reject(new Error(`Host exited ${code}: ${diagnostics}`)) })
      lines.on('line', line => {
        if (!line.startsWith('RAINY_CONTROL ')) return
        const value = JSON.parse(line.slice(14))
        if (value.type === 'ready') { clearTimeout(deadline); resolveReady(value) }
        else if (value.type === 'fatal') { clearTimeout(deadline); reject(new Error(value.message)) }
      })
    })
    browser = await chromium.launch({ channel: 'msedge', headless: true })
    context = await browser.newContext({ viewport })
    const origin = new URL(ready.url).origin
    const blocked = []
    await context.route('**/*', route => {
      if (new URL(route.request().url()).origin === origin) return route.continue()
      blocked.push(new URL(route.request().url()).hostname)
      return route.abort()
    })
    await context.request.get(ready.url)
    return { browser, context, ready, origin, home: privateHome, blocked, stop }
  } catch (error) { await stop(); throw error }
}
