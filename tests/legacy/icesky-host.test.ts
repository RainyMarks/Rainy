/** Real JSON storage and HTTP resource behavior behind the human workbench. */
import { createHash } from 'node:crypto'
import { createServer, request as httpRequest } from 'node:http'
import type { IncomingMessage, ServerResponse } from 'node:http'
import { mkdir, mkdtemp, readFile, rename, rm, rmdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { brandString } from '@deepseek-ai/dsh-brand'
import Storage from '@deepseek-ai/dsh-storage'
import { DomainFacility } from '@deepseek-ai/dsh-storage-domain'
import { JsonStorageBackend } from '@deepseek-ai/dsh-storage-json'
import { SessionId } from '@deepseek-ai/dsh-session'
import { createIceSkyDraftHandler, iceSkyDraftSpec, IceSkyDraftStore } from '../src/icesky-state.ts'
import type { IceSkyDraftEnvelope, IceSkyDraftScope } from '../src/icesky-state.ts'
import { IceSkyStaticAssets } from '../src/icesky-static.ts'
import { createIceSkyProxyHandler } from '../src/icesky-proxy.ts'

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
  const ctx = new Context()
  await ctx.plugin(Storage)
  const backend = new JsonStorageBackend(root)
  const unregister = ctx.storage.backend.register('json', backend)
  const facility = new DomainFacility(ctx, { backend: 'json', routes: {} })
  const domain = await facility.open(iceSkyDraftSpec)
  const store = new IceSkyDraftStore(domain)
  cleanups.push(async () => { await store.close(); unregister(); await backend.close(); await ctx.fiber.dispose() })
  return { root, store, domain, facility }
}

async function httpFixture(handler: (request: IncomingMessage, response: ServerResponse) => Promise<void>): Promise<string> {
  const server = createServer((request, response) => {
    void handler(request, response).catch((error: unknown) => { response.destroy(error instanceof Error ? error : undefined) })
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
  it('reopens a complete multi-tool draft from the real single JSON backend', async () => {
    const { store, root, facility } = await storeFixture()
    const text = '普通文字 😀\n'.repeat(18000)
    expect(store.get(standalone)).toEqual({ version: 1, revision: 0, data: {} })
    await store.replace(standalone, 0, data(text))
    const persisted: unknown = JSON.parse(await readFile(join(root, 'rainy_icesky.json'), 'utf8'))
    expect(persisted).toMatchObject({ unit: { name: 'rainy_icesky', version: 1 },
      tables: { drafts: { standalone: { data: { tools: { tokenizer: { fields: { tokenizerInput: text } } } } } } } })
    await store.close()
    const reopened = new IceSkyDraftStore(await facility.open(iceSkyDraftSpec))
    cleanups.push(() => reopened.close())
    expect(reopened.get(standalone)).toEqual({ version: 1, revision: 1, data: data(text) })
  })

  it('compares concurrent saves at their serialized commit slots', async () => {
    const { store, domain } = await storeFixture()
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    const table = domain.table('drafts')
    const put = table.put.bind(table)
    vi.spyOn(table, 'put').mockImplementationOnce(async (scope, value) => {
      entered.resolve(undefined); await release.promise; await put(scope, value)
    })
    const first = store.replace(standalone, 0, data('first'))
    await entered.promise
    const second = store.replace(standalone, 0, data('stale'))
    release.resolve(undefined)
    await expect(first).resolves.toMatchObject({ revision: 1 })
    await expect(second).rejects.toMatchObject({ current: { revision: 1, data: data('first') } })
    expect(store.get(standalone).data).toEqual(data('first'))
  })

  it('preserves disk and memory after a failed atomic file publication', async () => {
    const { store, root } = await storeFixture()
    await store.replace(standalone, 0, data('committed'))
    const path = join(root, 'rainy_icesky.json')
    const backup = join(root, 'rainy_icesky.committed.json')
    const before = await readFile(path)
    await rename(path, backup)
    await mkdir(path)
    try {
      await expect(store.replace(standalone, 1, data('rejected'))).rejects.toThrow()
      expect(store.get(standalone)).toEqual({ version: 1, revision: 1, data: data('committed') })
      expect(await readFile(backup)).toEqual(before)
    } finally { await rmdir(path); await rename(backup, path) }
    await expect(store.replace(standalone, 1, data('recovered'))).resolves.toMatchObject({ revision: 2 })
  })

  it('keeps clear tombstones so late requests cannot restore deleted data', async () => {
    const { store } = await storeFixture()
    await store.replace(standalone, 0, data('before clear'))
    expect(await store.replace(standalone, 1, {})).toEqual({ version: 1, revision: 2, data: {} })
    await expect(store.replace(standalone, 1, data('late save'))).rejects.toMatchObject({ current: { revision: 2, data: {} } })
  })

  it('drains admitted saves on close and rejects new ones', async () => {
    const { store, domain, root } = await storeFixture()
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    const table = domain.table('drafts')
    const put = table.put.bind(table)
    vi.spyOn(table, 'put').mockImplementationOnce(async (scope, value) => {
      entered.resolve(undefined); await release.promise; await put(scope, value)
    })
    const pending = store.replace(standalone, 0, data('drained'))
    await entered.promise
    const closing = store.close()
    await expect(store.replace(standalone, 0, {})).rejects.toThrow('closing')
    release.resolve(undefined)
    await pending
    await closing
    const persisted: unknown = JSON.parse(await readFile(join(root, 'rainy_icesky.json'), 'utf8'))
    expect(persisted).toMatchObject({ tables: { drafts: { standalone: { revision: 1 } } } })
  })

  it('authenticates every verb, admits cold Session identities and isolates scopes', async () => {
    const { store } = await storeFixture()
    const inspected: string[] = []
    const handler = createIceSkyDraftHandler(store, {
      admit: request => request.headers.cookie === 'fixture=yes' ? undefined : 403, maxDraftBytes: 8 * 1024 * 1024,
      sessionExists: async (id) => { inspected.push(id); return id === SessionId('cold-session') },
    })
    const origin = await httpFixture(handler)
    const request = (scope: string, method = 'GET', body?: object, authenticated = true) => fetch(`${origin}/rainy/icesky/state?scope=${encodeURIComponent(scope)}`, {
      method, headers: authenticated ? { cookie: 'fixture=yes', 'content-type': 'application/json' } : {}, body: body === undefined ? undefined : JSON.stringify(body),
    })
    for (const method of ['GET', 'PUT', 'DELETE']) expect((await request('session:cold-session', method, method === 'GET' ? undefined : { baseRevision: 0, data: {} }, false)).status).toBe(403)
    expect(inspected).toEqual([])
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
  })

  it('rejects credentials, binary file state, malformed scopes and over-budget JSON', async () => {
    const { store } = await storeFixture()
    const origin = await httpFixture(createIceSkyDraftHandler(store, {
      admit: () => undefined, sessionExists: async () => true, maxDraftBytes: 1024,
    }))
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
})

async function assetFixture() {
  const root = await temporaryRoot()
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
  const owner = await IceSkyStaticAssets.open(root)
  cleanups.push(() => owner.close())
  const origin = await httpFixture((request, response) => owner.handle(request, response, { admit: request => request.headers.cookie === 'fixture=yes' ? undefined : 403, maxAgeSeconds: 31536000 }))
  return { root, owner, origin, version, content }
}

describe('IceSky content-versioned resources', () => {
  it('injects a content-versioned base, preserves embedding and authenticates conditional responses', async () => {
    const { origin, version } = await assetFixture()
    const response = await fetch(`${origin}/rainy/icesky/index.html?embed=rainy`, { headers: { cookie: 'fixture=yes' } })
    expect(response.headers.get('cache-control')).toBe('private, no-cache')
    const html = await response.text()
    expect(html).toContain(`<base href="/rainy/icesky/v/${version}/">`)
    expect(html.indexOf('<base ')).toBeLessThan(html.indexOf('<script '))
    expect(html).toContain('css/rainy-embed.css')
    const cached = await fetch(`${origin}/rainy/icesky/index.html?embed=rainy`, { headers: { cookie: 'fixture=yes', 'if-none-match': response.headers.get('etag')! } })
    expect(cached.status).toBe(304)
    expect(await cached.text()).toBe('')
    const denied = await fetch(`${origin}/rainy/icesky/index.html`, { headers: { 'if-none-match': response.headers.get('etag')! } })
    expect(denied.status).toBe(403)
  })

  it('streams exact asset bytes and serves HEAD or 304 without reading a file body', async () => {
    const { root, origin, version, content } = await assetFixture()
    const url = `${origin}/rainy/icesky/v/${version}/js/local.js`
    const response = await fetch(url, { headers: { cookie: 'fixture=yes' } })
    expect(response.headers.get('cache-control')).toBe('private, max-age=31536000, immutable')
    expect(Buffer.from(await response.arrayBuffer())).toEqual(content['js/local.js'])
    await rm(join(root, 'js/local.js'))
    const head = await fetch(url, { method: 'HEAD', headers: { cookie: 'fixture=yes' } })
    expect(head.status).toBe(200)
    expect(head.headers.get('content-length')).toBe(String(content['js/local.js'].length))
    expect(await head.text()).toBe('')
    const cached = await fetch(url, { headers: { cookie: 'fixture=yes', 'if-none-match': response.headers.get('etag')! } })
    expect(cached.status).toBe(304)
    expect((await fetch(url, { headers: { cookie: 'fixture=yes' } })).status).toBe(404)
  })

  it('serves offline worker MIME types without HTTP-decompressing OCR data', async () => {
    const { origin, version } = await assetFixture()
    const headers = { cookie: 'fixture=yes' }
    expect((await fetch(`${origin}/rainy/icesky/v/${version}/js/local.wasm`, { headers })).headers.get('content-type')).toBe('application/wasm')
    const model = await fetch(`${origin}/rainy/icesky/v/${version}/js/eng.traineddata.gz`, { headers })
    expect(model.headers.get('content-type')).toBe('application/gzip')
    expect(model.headers.get('content-encoding')).toBeNull()
    expect(model.status).toBe(200)
    expect((await fetch(`${origin}/rainy/icesky/v/${'0'.repeat(64)}/js/local.js`, { headers })).status).toBe(410)
    expect((await fetch(`${origin}/rainy/icesky/js/%2e%2e%2fprivate.js`, { headers })).status).toBe(404)
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
    const upstream = await httpFixture(async (request, response) => {
      const chunks: Buffer[] = []
      for await (const chunk of request) chunks.push(chunk as Buffer)
      received = Buffer.concat(chunks).toString('utf8')
      response.writeHead(200, { 'Content-Type': 'application/json' })
      response.end('{}')
    })
    const origin = await httpFixture(createIceSkyProxyHandler('openai', 'chat', request => request.headers.cookie === 'fixture=yes' ? undefined : 403))
    const body = Buffer.from(JSON.stringify({ messages: [{ role: 'user', content: '雨天测试' }] }))
    const split = body.indexOf(Buffer.from('雨')) + 1
    const status = await new Promise<number | undefined>((resolve, reject) => {
      const request = httpRequest(`${origin}/api/openai/chat`, { method: 'POST', headers: { cookie: 'fixture=yes', 'x-openai-base-url': upstream } }, (response) => {
        response.resume()
        response.once('end', () => { resolve(response.statusCode) })
      })
      request.once('error', reject)
      request.write(body.subarray(0, split))
      setTimeout(() => { request.end(body.subarray(split)) }, 50)
    })
    expect(status).toBe(200)
    expect(received).toBe(body.toString('utf8'))
  })
})
