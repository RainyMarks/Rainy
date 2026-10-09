/** IceSky drafts, content-versioned resources, model relay and their routes on the Host server. */
import { createHash } from 'node:crypto'
import { createServer, request as httpRequest } from 'node:http'
import type { IncomingMessage, ServerResponse } from 'node:http'
import { mkdir, mkdtemp, readFile, rm, rmdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { HostEnvironment } from '../../src/host/env.ts'
import { writeJson } from '../../src/host/files.ts'
import { chatExists, installIceSky } from '../../src/host/icesky/index.ts'
import { createIceSkyProxyHandler } from '../../src/host/icesky/proxy.ts'
import { createIceSkyDraftHandler, IceSkyDraftStore, type IceSkyDraftEnvelope, type IceSkyDraftScope } from '../../src/host/icesky/state.ts'
import { IceSkyStaticAssets } from '../../src/host/icesky/static.ts'
import { HostServer } from '../../src/host/server.ts'

const cleanups: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
  vi.restoreAllMocks()
})

async function temporaryRoot(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), 'rainy-icesky-host-'))
  cleanups.push(() => rm(root, { recursive: true, force: true }))
  return root
}

async function storeFixture() {
  const root = await temporaryRoot()
  const path = join(root, 'icesky', 'state.json')
  const gate: { hold?: { entered: () => void; release: Promise<void> } } = {}
  const write = async (target: string, value: unknown): Promise<void> => {
    const hold = gate.hold
    gate.hold = undefined
    if (hold !== undefined) { hold.entered(); await hold.release }
    await writeJson(target, value)
  }
  const store = await IceSkyDraftStore.open(path, write)
  cleanups.push(() => store.close())
  const holdNextWrite = () => {
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    gate.hold = { entered: () => { entered.resolve(undefined) }, release: release.promise.then(() => undefined) }
    return { entered: entered.promise, release: () => { release.resolve(undefined) } }
  }
  return { root, path, store, holdNextWrite, reopen: () => IceSkyDraftStore.open(path) }
}

async function httpFixture(handler: (request: IncomingMessage, response: ServerResponse, url: URL) => Promise<void>): Promise<string> {
  const server = createServer((request, response) => {
    void handler(request, response, new URL(request.url ?? '/', 'http://localhost')).catch((error: unknown) => { response.destroy(error instanceof Error ? error : undefined) })
  })
  cleanups.push(() => new Promise<void>((resolve, reject) => {
    server.close((error) => { if (error) reject(error); else resolve() })
    server.closeAllConnections()
  }))
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', () => { server.removeListener('error', reject); resolve() }) })
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('HTTP fixture did not acquire a TCP listener.')
  return `http://127.0.0.1:${address.port}`
}

const standalone = brandString<IceSkyDraftScope>('standalone')
const data = (text: string): IceSkyDraftEnvelope['data'] => ({ shared: { selectedTool: 'tokenizer' }, tools: { tokenizer: { fields: { tokenizerInput: text }, files: {} } }, legacyMigrated: true })

describe('IceSky durable drafts', () => {
  it('reopens a complete multi-tool draft from its JSON file', async () => {
    const { store, path, reopen } = await storeFixture()
    const text = '普通文字 😀\n'.repeat(18000)
    expect(store.get(standalone)).toEqual({ version: 1, revision: 0, data: {} })
    await store.replace(standalone, 0, data(text))
    expect(JSON.parse(await readFile(path, 'utf8'))).toMatchObject({ version: 1,
      drafts: { standalone: { revision: 1, data: { tools: { tokenizer: { fields: { tokenizerInput: text } } } } } } })
    await store.close()
    const reopened = await reopen()
    cleanups.push(() => reopened.close())
    expect(reopened.get(standalone)).toEqual({ version: 1, revision: 1, data: data(text) })
  })

  it('compares concurrent saves at their serialized commit slots', async () => {
    const { store, holdNextWrite } = await storeFixture()
    const hold = holdNextWrite()
    const first = store.replace(standalone, 0, data('first'))
    await hold.entered
    const second = store.replace(standalone, 0, data('stale'))
    hold.release()
    await expect(first).resolves.toMatchObject({ revision: 1 })
    await expect(second).rejects.toMatchObject({ current: { revision: 1, data: data('first') } })
    expect(store.get(standalone).data).toEqual(data('first'))
  })

  it('preserves disk and memory after a failed atomic file publication', async () => {
    const { store, path } = await storeFixture()
    await store.replace(standalone, 0, data('committed'))
    const before = await readFile(path)
    await rm(path)
    await mkdir(path)
    try {
      await expect(store.replace(standalone, 1, data('rejected'))).rejects.toThrow()
      expect(store.get(standalone)).toEqual({ version: 1, revision: 1, data: data('committed') })
    } finally { await rmdir(path); await writeFile(path, before) }
    await expect(store.replace(standalone, 1, data('recovered'))).resolves.toMatchObject({ revision: 2 })
  })

  it('keeps clear tombstones so late requests cannot restore deleted data', async () => {
    const { store } = await storeFixture()
    await store.replace(standalone, 0, data('before clear'))
    expect(await store.replace(standalone, 1, {})).toEqual({ version: 1, revision: 2, data: {} })
    await expect(store.replace(standalone, 1, data('late save'))).rejects.toMatchObject({ current: { revision: 2, data: {} } })
  })

  it('drains admitted saves on close and rejects new ones', async () => {
    const { store, holdNextWrite, path } = await storeFixture()
    const hold = holdNextWrite()
    const pending = store.replace(standalone, 0, data('drained'))
    await hold.entered
    const closing = store.close()
    await expect(store.replace(standalone, 0, {})).rejects.toThrow('closing')
    hold.release()
    await pending
    await closing
    expect(JSON.parse(await readFile(path, 'utf8'))).toMatchObject({ drafts: { standalone: { revision: 1 } } })
  })

  it('refuses an unsupported draft file without changing it', async () => {
    const { root } = await storeFixture()
    const path = join(root, 'future.json')
    await writeFile(path, '{"version":2,"drafts":{}}')
    await expect(IceSkyDraftStore.open(path)).rejects.toThrow()
    expect(await readFile(path, 'utf8')).toBe('{"version":2,"drafts":{}}')
  })

  it('admits chat scopes, isolates scopes and answers conflicts with the committed value', async () => {
    const { store } = await storeFixture()
    const inspected: string[] = []
    const origin = await httpFixture(createIceSkyDraftHandler(store, { maxDraftBytes: 8 * 1024 * 1024,
      sessionExists: async (id) => { inspected.push(id); return id === 'cold-session' } }))
    const request = (scope: string, method = 'GET', body?: object) => fetch(`${origin}/rainy/icesky/state?scope=${encodeURIComponent(scope)}`, {
      method, headers: { 'content-type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body),
    })
    expect(await (await request('session:cold-session')).json()).toEqual({ version: 1, revision: 0, data: {} })
    expect((await request('session:missing', 'PUT', { baseRevision: 0, data: data('denied') })).status).toBe(404)
    expect((await request('session:cold-session', 'PUT', { baseRevision: 0, data: data('cold draft') })).status).toBe(200)
    expect(await (await request('standalone')).json()).toEqual({ version: 1, revision: 0, data: {} })
    const stale = await request('session:cold-session', 'PUT', { baseRevision: 0, data: data('stale') })
    expect(stale.status).toBe(409)
    expect(await stale.json()).toMatchObject({ revision: 1, error: { code: 'revision-conflict' } })
    const cleared = await request('session:cold-session', 'DELETE', { baseRevision: 1 })
    expect(cleared.headers.get('cache-control')).toBe('no-store')
    expect(await cleared.json()).toEqual({ version: 1, revision: 2, data: {} })
    expect(inspected.every(id => id === 'cold-session' || id === 'missing')).toBe(true)
    expect((await fetch(`${origin}/rainy/icesky/state?scope=standalone`, { method: 'POST' })).status).toBe(405)
  })

  it('rejects credentials, binary file state, malformed scopes and over-budget JSON', async () => {
    const { store } = await storeFixture()
    const origin = await httpFixture(createIceSkyDraftHandler(store, { sessionExists: async () => true, maxDraftBytes: 1024 }))
    const send = (body: object, query = 'scope=standalone') => fetch(`${origin}/rainy/icesky/state?${query}`, { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) })
    for (const illegal of [
      { shared: { nested: { openai_api_key: 'fixture-secret' } } },
      { tools: { tokenizer: { fields: { apiKey: 'fixture-secret' } } } },
      { tools: { tokenizer: { files: { uploaded: { name: 'safe.txt', bytes: [1, 2, 3] } } } } },
      { unsupported: {} },
    ]) expect((await send({ baseRevision: 0, data: illegal })).status).toBe(400)
    expect((await send({ baseRevision: 0, data: data('x'.repeat(2000)) })).status).toBe(413)
    expect((await send({ baseRevision: 0, data: {} }, 'scope=standalone&scope=standalone')).status).toBe(400)
    expect((await send({ baseRevision: 0, data: {} }, 'scope=session%3A..%2Fother')).status).toBe(400)
    expect(store.get(standalone).revision).toBe(0)
  })

  it('answers 503 when the draft file could not be read', async () => {
    const origin = await httpFixture(createIceSkyDraftHandler(undefined, { sessionExists: async () => true, maxDraftBytes: 1024 }))
    const response = await fetch(`${origin}/rainy/icesky/state?scope=standalone`)
    expect(response.status).toBe(503)
    expect(await response.json()).toMatchObject({ error: { code: 'draft-storage-unavailable' } })
  })

  it('recognizes a chat by its log file and rejects identities that are not file names', async () => {
    const root = await temporaryRoot()
    await mkdir(join(root, 'chats'))
    await writeFile(join(root, 'chats', 'a1b2-c3.jsonl'), '{}\n')
    expect(await chatExists(root, 'a1b2-c3')).toBe(true)
    expect(await chatExists(root, 'missing')).toBe(false)
    for (const id of ['../chats/a1b2-c3', 'a:b', '']) expect(await chatExists(root, id)).toBe(false)
  })
})

async function writeAssets(root: string) {
  const content: Record<string, Buffer> = {
    'index.html': Buffer.from('<html><head><script src="js/local.js"></script></head><body>Local workbench</body></html>'),
    'css/rainy-embed.css': Buffer.from('body{padding:0}'),
    'js/local.js': Buffer.from('globalThis.fixture = true;'),
    'js/local.wasm': Buffer.from([0, 97, 115, 109]),
    'js/eng.traineddata.gz': Buffer.from([31, 139, 8, 0]),
  }
  const files: Record<string, { sha256: string; size: number; type: string }> = {}
  for (const [name, bytes] of Object.entries(content)) {
    await mkdir(join(root, name, '..'), { recursive: true })
    await writeFile(join(root, name), bytes)
    files[name] = { sha256: createHash('sha256').update(bytes).digest('hex'), size: bytes.length, type: name.endsWith('.html') ? 'text/html' : name.endsWith('.css') ? 'text/css' : name.endsWith('.wasm') ? 'application/wasm' : name.endsWith('.gz') ? 'application/gzip' : 'text/javascript' }
  }
  const version = createHash('sha256').update(JSON.stringify(files)).digest('hex')
  await writeFile(join(root, 'assets-manifest.json'), JSON.stringify({ format: 1, version, files }))
  return { version, content }
}

async function assetFixture() {
  const root = await temporaryRoot()
  const { version, content } = await writeAssets(root)
  const owner = await IceSkyStaticAssets.open(root)
  cleanups.push(() => owner.close())
  const origin = await httpFixture((request, response) => owner.handle(request, response, { maxAgeSeconds: 31536000 }))
  return { root, owner, origin, version, content }
}

describe('IceSky content-versioned resources', () => {
  it('injects a content-versioned base, preserves embedding and answers conditional requests', async () => {
    const { origin, version } = await assetFixture()
    const response = await fetch(`${origin}/rainy/icesky/index.html?embed=rainy`)
    expect(response.headers.get('cache-control')).toBe('private, no-cache')
    const html = await response.text()
    expect(html).toContain(`<base href="/rainy/icesky/v/${version}/">`)
    expect(html.indexOf('<base ')).toBeLessThan(html.indexOf('<script '))
    expect(html).toContain('css/rainy-embed.css')
    expect(await (await fetch(`${origin}/rainy/icesky/index.html`)).text()).not.toContain('rainy-embed.css')
    const cached = await fetch(`${origin}/rainy/icesky/index.html?embed=rainy`, { headers: { 'if-none-match': response.headers.get('etag')! } })
    expect(cached.status).toBe(304)
    expect(await cached.text()).toBe('')
  })

  it('streams exact asset bytes and serves HEAD or 304 without reading a file body', async () => {
    const { root, origin, version, content } = await assetFixture()
    const url = `${origin}/rainy/icesky/v/${version}/js/local.js`
    const response = await fetch(url)
    expect(response.headers.get('cache-control')).toBe('private, max-age=31536000, immutable')
    expect(Buffer.from(await response.arrayBuffer())).toEqual(content['js/local.js'])
    await rm(join(root, 'js/local.js'))
    const head = await fetch(url, { method: 'HEAD' })
    expect(head.status).toBe(200)
    expect(head.headers.get('content-length')).toBe(String(content['js/local.js']!.length))
    expect(await head.text()).toBe('')
    const cached = await fetch(url, { headers: { 'if-none-match': response.headers.get('etag')! } })
    expect(cached.status).toBe(304)
    expect((await fetch(url)).status).toBe(404)
  })

  it('serves offline worker MIME types without HTTP-decompressing OCR data', async () => {
    const { origin, version } = await assetFixture()
    expect((await fetch(`${origin}/rainy/icesky/v/${version}/js/local.wasm`)).headers.get('content-type')).toBe('application/wasm')
    const model = await fetch(`${origin}/rainy/icesky/v/${version}/js/eng.traineddata.gz`)
    expect(model.headers.get('content-type')).toBe('application/gzip')
    expect(model.headers.get('content-encoding')).toBeNull()
    expect(model.status).toBe(200)
    expect((await fetch(`${origin}/rainy/icesky/v/${'0'.repeat(64)}/js/local.js`)).status).toBe(410)
    expect((await fetch(`${origin}/rainy/icesky/js/%2e%2e%2fprivate.js`)).status).toBe(404)
  })

  it('refuses a stale index instead of mixing resource versions', async () => {
    const { root } = await assetFixture()
    await writeFile(join(root, 'index.html'), '<html><head></head><body>changed</body></html>')
    await expect(IceSkyStaticAssets.open(root)).rejects.toThrow('differs')
  })
})

describe('IceSky model relay', () => {
  it('relays a request body whose multi-byte characters span network chunks', async () => {
    let received = ''
    let authorization: string | undefined
    const upstream = await httpFixture(async (request, response) => {
      const chunks: Buffer[] = []
      for await (const chunk of request) chunks.push(chunk as Buffer)
      received = Buffer.concat(chunks).toString('utf8')
      authorization = request.headers.authorization
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end('{}')
    })
    const origin = await httpFixture(createIceSkyProxyHandler('openai', 'chat'))
    const body = Buffer.from(JSON.stringify({ messages: [{ role: 'user', content: '雨天测试' }] }))
    const split = body.indexOf(Buffer.from('雨')) + 1
    const status = await new Promise<number | undefined>((resolve, reject) => {
      const request = httpRequest(`${origin}/api/openai/chat`, { method: 'POST', headers: { 'x-openai-base-url': upstream, 'x-openai-api-key': 'sk-test' } }, (response) => {
        response.resume()
        response.once('end', () => { resolve(response.statusCode) })
      })
      request.once('error', reject)
      request.write(body.subarray(0, split))
      setTimeout(() => { request.end(body.subarray(split)) }, 50)
    })
    expect(status).toBe(200)
    expect(received).toBe(body.toString('utf8'))
    expect(authorization).toBe('Bearer sk-test')
  })

  it('rejects wrong methods, invalid JSON and credentials in the base URL', async () => {
    const origin = await httpFixture(createIceSkyProxyHandler('anthropic', 'chat'))
    expect((await fetch(`${origin}/api/anthropic/chat`)).status).toBe(405)
    const invalid = await fetch(`${origin}/api/anthropic/chat`, { method: 'POST', body: '{', headers: { 'x-anthropic-base-url': 'http://127.0.0.1:9' } })
    expect(invalid.status).toBe(502)
    expect(await invalid.json()).toEqual({ error: { message: '模型请求 JSON 无效。' } })
    const credentials = await fetch(`${origin}/api/anthropic/chat`, { method: 'POST', body: '{}', headers: { 'x-anthropic-base-url': 'http://user:pass@127.0.0.1:9' } })
    expect(await credentials.json()).toEqual({ error: { message: '模型接口地址必须是 HTTP(S) Base URL。' } })
  })
})

describe('IceSky routes on the Host server', () => {
  it('serves resources, drafts and relays only to the authenticated renderer', async () => {
    const root = await temporaryRoot()
    const env: HostEnvironment = {
      version: '0.0.0-test', home: join(root, 'home'), appRoot: process.cwd(), resources: join(root, 'resources'),
      platform: process.platform === 'win32' ? 'win32' : 'linux', executionTargetId: 'wsl:test', carrierStateRoot: join(root, 'carrier'),
      toolchainRoot: join(root, 'components'), tmp: join(root, 'tmp'), port: 0,
    }
    const { version } = await writeAssets(join(env.resources, 'icesky'))
    await mkdir(join(env.home, 'chats'), { recursive: true })
    await writeFile(join(env.home, 'chats', 'chat-1.jsonl'), '{}\n')
    const server = new HostServer({ port: 0, rendererRoot: join(root, 'renderer'), injectedGlobals: () => ({}), log: vi.fn() })
    await installIceSky({ env, server, log: vi.fn() })
    await server.listen()
    cleanups.push(() => server.close())
    const origin = `http://127.0.0.1:${server.port}`
    const cookie = ((await fetch(server.launchUrl(), { redirect: 'manual' })).headers.get('set-cookie') ?? '').split(';')[0] ?? ''
    const get = (path: string, headers: Record<string, string> = { cookie }) => fetch(`${origin}${path}`, { headers })
    expect((await get('/rainy/icesky/index.html', {})).status).toBe(403)
    expect((await get('/rainy/icesky/state?scope=standalone', {})).status).toBe(403)
    expect((await get('/api/openai/models', {})).status).toBe(403)
    expect(await (await get('/rainy/icesky')).text()).toContain(`/rainy/icesky/v/${version}/`)
    expect((await get(`/rainy/icesky/v/${version}/js/local.js`)).status).toBe(200)
    const saved = await fetch(`${origin}/rainy/icesky/state?scope=session%3Achat-1`, { method: 'PUT', headers: { cookie, 'content-type': 'application/json' },
      body: JSON.stringify({ baseRevision: 0, data: data('kept') }) })
    expect(saved.status).toBe(200)
    expect((await get('/rainy/icesky/state?scope=session%3Aother')).status).toBe(404)
    expect(JSON.parse(await readFile(join(env.home, 'icesky', 'state.json'), 'utf8'))).toMatchObject({ drafts: { 'session:chat-1': { revision: 1 } } })
  })
})
