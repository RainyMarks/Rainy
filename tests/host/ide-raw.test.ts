/** The IDE file byte route: containment, ranges, validators and inert responses. */
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { ideRawUrl } from '../../src/shared/ide-preview.ts'
import { RainyIdeFiles, resolveIdeFilesConfig } from '../../src/host/ide/files.ts'
import { createIdeRawRoute, parseByteRange } from '../../src/host/ide/raw.ts'
import { Projects } from '../../src/host/projects.ts'

const cleanups: (() => Promise<void>)[] = []
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup() })

const PNG = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52])

async function fixture() {
  const temporary = await realpath(await mkdtemp(join(tmpdir(), 'rainy-ide-raw-')))
  cleanups.push(() => rm(temporary, { recursive: true, force: true }))
  const root = join(temporary, '项目')
  const outside = join(temporary, 'outside')
  await mkdir(join(root, 'images'), { recursive: true })
  await mkdir(outside)
  await writeFile(join(root, 'images', 'logo.png'), PNG)
  await writeFile(join(root, 'empty.bin'), Buffer.alloc(0))
  await writeFile(join(outside, 'secret.txt'), 'outside')
  await symlink(outside, join(root, 'escape'), process.platform === 'win32' ? 'junction' : 'dir')
  const projects = new Projects(join(temporary, 'home', 'projects.json'))
  await projects.load()
  const workspaceId = (await projects.open(root)).id
  const files = new RainyIdeFiles({ projects, config: resolveIdeFilesConfig({}) })
  cleanups.push(() => files.close())
  const lifetime = new AbortController()
  const route = createIdeRawRoute(files, { assertUsable: () => undefined, signal: lifetime.signal })
  const server = createServer((request: IncomingMessage, response: ServerResponse) => {
    void route(request, response, new URL(request.url ?? '/', 'http://127.0.0.1'))
  })
  await new Promise<void>((resolve) => { server.listen(0, '127.0.0.1', resolve) })
  cleanups.push(() => new Promise<void>((resolve) => { lifetime.abort(); server.closeAllConnections(); server.close(() => { resolve() }) }))
  const address = server.address()
  if (address === null || typeof address === 'string') throw new Error('No server address')
  const url = (path: string, version?: string) => `http://127.0.0.1:${address.port}${ideRawUrl({ workspaceId, path, version })}`
  return { files, workspaceId, url }
}

describe('parseByteRange', () => {
  it('reads one bytes range, a suffix, an open end, and rejects ranges past the end', () => {
    expect(parseByteRange('bytes=2-5', 16)).toEqual({ start: 2, end: 5 })
    expect(parseByteRange('bytes=10-', 16)).toEqual({ start: 10, end: 15 })
    expect(parseByteRange('bytes=-4', 16)).toEqual({ start: 12, end: 15 })
    expect(parseByteRange('bytes=4-100', 16)).toEqual({ start: 4, end: 15 })
    expect(parseByteRange('bytes=16-', 16)).toBe('unsatisfiable')
    expect(parseByteRange('bytes=5-2', 16)).toBe('unsatisfiable')
    expect(parseByteRange('bytes=0-1,4-5', 16)).toBeUndefined()
    expect(parseByteRange(undefined, 16)).toBeUndefined()
  })
})

describe('IDE file byte route', () => {
  it('serves a project file with its type, version and inert headers', async () => {
    const { url } = await fixture()
    const response = await fetch(url('images/logo.png'))
    expect(response.status).toBe(200)
    expect(response.headers.get('content-type')).toBe('image/png')
    expect(response.headers.get('accept-ranges')).toBe('bytes')
    expect(response.headers.get('content-security-policy')).toMatch(/^sandbox;/u)
    expect(response.headers.get('x-content-type-options')).toBe('nosniff')
    expect(response.headers.get('cache-control')).toBe('private, no-cache')
    expect(Buffer.from(await response.arrayBuffer())).toEqual(PNG)
    const etag = response.headers.get('etag')
    expect(etag).toMatch(/^"[a-f0-9]{64}"$/u)
    expect((await fetch(url('images/logo.png'), { headers: { 'If-None-Match': etag ?? '' } })).status).toBe(304)
    expect((await fetch(url('images/logo.png', 'v1'))).headers.get('cache-control')).toBe('private, max-age=3600')
  })

  it('answers single ranges for seeking and paging', async () => {
    const { url } = await fixture()
    const partial = await fetch(url('images/logo.png'), { headers: { Range: 'bytes=1-3' } })
    expect(partial.status).toBe(206)
    expect(partial.headers.get('content-range')).toBe(`bytes 1-3/${PNG.length}`)
    expect(Buffer.from(await partial.arrayBuffer())).toEqual(PNG.subarray(1, 4))
    const tail = await fetch(url('images/logo.png'), { headers: { Range: 'bytes=-4' } })
    expect(Buffer.from(await tail.arrayBuffer())).toEqual(PNG.subarray(PNG.length - 4))
    const beyond = await fetch(url('images/logo.png'), { headers: { Range: 'bytes=100-' } })
    expect(beyond.status).toBe(416)
    expect(beyond.headers.get('content-range')).toBe(`bytes */${PNG.length}`)
    const head = await fetch(url('images/logo.png'), { method: 'HEAD' })
    expect(head.headers.get('content-length')).toBe(String(PNG.length))
    const empty = await fetch(url('empty.bin'))
    expect(empty.status).toBe(200)
    expect(empty.headers.get('content-type')).toBe('application/octet-stream')
    expect((await empty.arrayBuffer()).byteLength).toBe(0)
  })

  it('refuses paths outside the project, directories and missing files', async () => {
    const { url } = await fixture()
    expect((await fetch(url('escape/secret.txt'))).status).toBe(404)
    expect((await fetch(url('../outside/secret.txt'))).status).toBe(400)
    expect((await fetch(url('images'))).status).toBe(404)
    expect((await fetch(url('images/missing.png'))).status).toBe(404)
    expect((await fetch(url('images/logo.png').replace('w=', 'x='))).status).toBe(400)
    expect((await fetch(url('images/logo.png'), { method: 'POST' })).status).toBe(405)
  })

  it('reads media files as metadata for their viewer without decoding them', async () => {
    const { files, workspaceId } = await fixture()
    const document = await files.handle({ op: 'files.read', workspaceId, path: 'images/logo.png' })
    expect(document).toMatchObject({ content: null, readOnlyReason: 'media', bytes: PNG.length })
    expect(document.preview).toBeUndefined()
  })
})
