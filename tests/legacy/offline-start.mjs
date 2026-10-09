/** Start the installed Host inside an isolated network namespace, fetch its UI, then shut it down. */
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import assert from 'node:assert/strict';

const network = await readFile('/proc/net/route', 'utf8');
assert(!network.split('\n').slice(1).some(line => /^\S+\s+00000000\s/.test(line)), 'Offline test requires a network namespace with no default route');
const home = await mkdtemp(join(tmpdir(), 'rainy-offline-'));
const started = performance.now();
const child = spawn(process.execPath, ['--expose-internals', process.argv[2]], {
  env: { ...process.env, RAINY_HOME: home, RAINY_CONFIGURE_DEEPSEEK: '0', RAINY_DETACHED: '0' }, stdio: ['pipe', 'pipe', 'pipe'],
});
const exited = new Promise(resolve => child.once('exit', code => resolve(code)));
let diagnostic = '';
child.stderr.on('data', data => { diagnostic = (diagnostic + String(data)).slice(-4000); });
const lines = createInterface({ input: child.stdout });
try {
  const ready = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('Offline Host startup timed out: ' + diagnostic)), 30000);
    child.once('error', reject);
    child.once('exit', code => { clearTimeout(timeout); reject(new Error(`Host exited ${code}: ${diagnostic}`)); });
    lines.on('line', line => {
      if (!line.startsWith('RAINY_CONTROL ')) return;
      const event = JSON.parse(line.slice(14));
      clearTimeout(timeout);
      if (event.type === 'ready') resolve(event); else reject(new Error(event.message));
    });
  });
  const startupMs = performance.now() - started;
  const response = await fetch(ready.url, { redirect: 'manual' });
  const cookie = response.headers.getSetCookie().map(item => item.split(';')[0]).join('; ');
  const page = response.status === 200 ? response : await fetch(new URL(ready.url).origin, { headers: { cookie } });
  assert.equal(page.status, 200);
  const html = await page.text();
  assert(html.includes('__RAINY_AGENT__') && html.includes('/rainy/panel.js'));
  const status = await fetch(new URL('/rainy/control', ready.url), { headers: { cookie } });
  assert.equal(status.status, 200);
  const state = await status.json();
  assert.deepEqual(state.tools, ['read', 'write', 'edit', 'bash']);
  assert.equal(state.models.length, 0);
  child.stdin.end('{"type":"stop"}\n');
  const code = await Promise.race([exited, new Promise((_, reject) => { const timeout = setTimeout(() => reject(new Error('Host did not stop after control EOF')), 10000); timeout.unref(); })]);
  assert.equal(code, 0);
  console.log(JSON.stringify({ kind: 'installed-host-no-network', startupMs, htmlBytes: Buffer.byteLength(html), defaultTools: state.tools, configuredModels: state.models.length, orderlyShutdown: 'passed' }));
} finally { lines.close(); if (child.exitCode === null) child.kill('SIGTERM'); }
