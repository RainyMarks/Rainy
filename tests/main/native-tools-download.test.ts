/** Per-tool downloads: signed channels, resumable pieces, unchanged-tool reuse, removal and storage maintenance. */
import { createHash, generateKeyPairSync, randomUUID, sign } from 'node:crypto'
import type { KeyObject } from 'node:crypto'
import { mkdtemp, mkdir, readdir, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, relative } from 'node:path'
import { c } from 'tar'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { NativeToolsDownloader } from '../src/native-tools-download.ts'
import type { NativeToolsDownloadOptions } from '../src/native-tools-download.ts'
import { createNativeToolPackInstaller } from '../src/toolpack.ts'
import { releasePublicKeyId } from '../src/release-trust.ts'
import { TOOL_CHANNEL_SIGNATURE_DOMAIN, TOOL_CHANNEL_URL } from '../src/native-tools-update.ts'
import { NativeToolsLibrary } from '../src/native-tools.ts'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { Branded } from '@deepseek-ai/dsh-brand'

const roots: string[] = []
const owners: NativeToolsDownloader[] = []
afterEach(async () => {
  vi.restoreAllMocks()
  await Promise.all(owners.splice(0).map(owner => owner.close()))
  for (const root of roots.splice(0)) {
    if (relative(tmpdir(), root).startsWith('..')) throw new Error('Test directory escaped temporary storage')
    await rm(root, { recursive: true, force: true })
  }
})
const hash = (value: string | Buffer): string => createHash('sha256').update(value).digest('hex')
const baseUrl = 'https://github.com/RainyMarks/RainyAgent/releases/download/v1.0.0-resources/'
const alpha = brandString<Branded<'NativeToolId'>>('alpha')
const href = (input: RequestInfo | URL): string => typeof input === 'string' ? input : input instanceof URL ? input.href : input.url
const beta = brandString<Branded<'NativeToolId'>>('beta')

const tool = (id: string, roots: string[]) => ({ id, category: 'misc', name: id, version: '1', roots,
  entry: { kind: 'gui', path: `tools/${id}/app.exe`, cwd: `tools/${id}`, args: [] } })

/** One signed revision with an archive per unit; the runtime is shared only by beta. */
async function pack(root: string, revision: number, content: Record<string, string>) {
  const name = `pack-${revision}`
  const input = join(root, name)
  const catalog = JSON.stringify({ version: 1, tools: [tool('alpha', ['tools/alpha']), tool('beta', ['tools/beta', 'runtime/windows/rt'])], revision })
  const all: Record<string, string> = { ...content, 'tools/manifest.json': catalog }
  for (const [path, value] of Object.entries(all)) {
    await mkdir(dirname(join(input, path)), { recursive: true })
    await writeFile(join(input, path), value)
  }
  const files = Object.entries(all).map(([path, value]) => ({ path, bytes: Buffer.byteLength(value), sha256: hash(value) }))
    .sort((left, right) => left.path < right.path ? -1 : left.path > right.path ? 1 : 0)
  const units = [{ path: 'runtime/windows/rt', kind: 'directory', preserve: [] }, { path: 'tools/alpha', kind: 'directory', preserve: [] },
    { path: 'tools/beta', kind: 'directory', preserve: [] }, { path: 'tools/manifest.json', kind: 'file', preserve: [] }] as const
  const served = new Map<string, Buffer>()
  const archives = []
  const downloads = []
  for (const unit of units) {
    const paths = files.filter(file => unit.kind === 'file' ? file.path === unit.path : file.path.startsWith(unit.path + '/')).map(file => file.path)
    const chunks: Buffer[] = []
    const options = { cwd: input, gzip: true, portable: true, noDirRecurse: true, mtime: new Date(0) }
    for await (const chunk of c(options, paths)) chunks.push(Buffer.from(chunk))
    const bytes = Buffer.concat(chunks)
    const file = `rainy-unit-${hash(JSON.stringify(paths.map(path => [path, all[path]]))).slice(0, 20)}.tar.gz`
    // The beta archive arrives in two pieces; the others in one.
    const parts = unit.path === 'tools/beta' ? [bytes.subarray(0, 10), bytes.subarray(10)] : [bytes]
    const pieces = parts.map((value, index) => {
      const piece = { file: `rainy-${hash(file).slice(0, 20)}.${String(index + 1).padStart(3, '0')}`, bytes: value.length, sha256: hash(value) }
      served.set(baseUrl + piece.file, value)
      return piece
    })
    archives.push({ unit: unit.path, file, bytes: bytes.length, sha256: hash(bytes) })
    downloads.push({ file, bytes: bytes.length, sha256: hash(bytes), baseUrl, pieces })
  }
  const id = hash(JSON.stringify({ files, units }))
  const metadata = { version: 2, id, format: 'tar.gz', unpackedBytes: files.reduce((sum, file) => sum + file.bytes, 0), files, units, archives }
  return { metadata, source: { version: 2, packId: id, archives: downloads }, catalog, served, revision }
}

type Pack = Awaited<ReturnType<typeof pack>>

function envelope(value: Pack, keyId: string, privateKey: KeyObject) {
  const payload = Buffer.from(JSON.stringify({ version: 2, revision: value.revision, releaseVersion: `1.0.${value.revision}`, keyId,
    source: value.source, metadata: value.metadata, catalog: value.catalog }))
  return { version: 2, payload: payload.toString('base64'),
    signature: sign(null, Buffer.concat([Buffer.from(TOOL_CHANNEL_SIGNATURE_DOMAIN), payload]), privateKey).toString('base64') }
}

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'rainy-online-tools-'))
  roots.push(root)
  const key = generateKeyPairSync('ed25519')
  const pem = key.publicKey.export({ type: 'spki', format: 'pem' }).toString()
  const keyId = releasePublicKeyId(pem)
  const keys = { version: 1 as const, keys: { [keyId]: pem } }
  const first = await pack(root, 1, { 'tools/alpha/app.exe': 'alpha one', 'tools/beta/app.exe': 'beta one', 'runtime/windows/rt/rt.dll': 'runtime one' })
  const second = await pack(root, 2, { 'tools/alpha/app.exe': 'alpha two', 'tools/beta/app.exe': 'beta one', 'runtime/windows/rt/rt.dll': 'runtime one' })
  const channelPath = join(root, 'bundled-channel.json')
  await writeFile(channelPath, JSON.stringify(envelope(first, keyId, key.privateKey)))
  let published: Pack = first
  const served = new Map([...first.served, ...second.served])
  const transport = vi.fn<typeof fetch>(async (input, options) => {
    const url = href(input)
    if (url === TOOL_CHANNEL_URL) return new Response(JSON.stringify(envelope(published, keyId, key.privateKey)))
    const value = served.get(url)
    if (value === undefined) throw new Error(`Unexpected download URL ${url}`)
    const range = new Headers(options?.headers).get('range')
    if (range) {
      const offset = Number(range.slice(6, -1))
      return new Response(new Uint8Array(value.subarray(offset)), { status: 206, headers: { 'content-range': `bytes ${offset}-${value.length - 1}/${value.length}` } })
    }
    return new Response(new Uint8Array(value))
  })
  const downloads = (): string[] => transport.mock.calls.map(([input]) => href(input)).filter(url => url !== TOOL_CHANNEL_URL)
  const publish = vi.fn()
  const install = createNativeToolPackInstaller({ availableBytes: async () => 1024 ** 4, assertNotBusy: async () => {}, move: rename })
  const options: NativeToolsDownloadOptions = { onlineRoot: join(root, 'online'), cacheRoot: join(root, 'cache'), channelPath, keys, publish,
    fetch: transport, install }
  const create = (overrides: Partial<NativeToolsDownloadOptions> = {}): NativeToolsDownloader => {
    const owner = new NativeToolsDownloader({ ...options, ...overrides })
    owners.push(owner)
    return owner
  }
  return { root, first, second, options, transport, downloads, publish, create,
    publishRevision: (value: Pack) => { published = value }, sign: (value: Pack) => envelope(value, keyId, key.privateKey) }
}

const piecesOf = (value: Pack, unit: string): string[] => {
  const file = value.metadata.archives.find(archive => archive.unit === unit)?.file
  return value.source.archives.find(archive => archive.file === file)?.pieces.map(piece => baseUrl + piece.file) ?? []
}

describe('per-tool downloads', () => {
  it('lists every tool with its download size before anything is installed, without network access', async () => {
    const h = await fixture()
    const inventory = await h.create().inventory()
    const bytes = (unit: string): number => h.first.metadata.archives.find(archive => archive.unit === unit)?.bytes ?? 0
    expect(inventory.root).toBe(h.options.onlineRoot)
    expect(Object.fromEntries(inventory.states)).toEqual({
      alpha: { installed: false, outdated: false, downloadBytes: bytes('tools/alpha') },
      beta: { installed: false, outdated: false, downloadBytes: bytes('tools/beta') + bytes('runtime/windows/rt') },
    })
    expect(h.transport).not.toHaveBeenCalled()
  })

  it('downloads one tool with its runtime, removes the used archives, and does nothing more when asked again', async () => {
    const h = await fixture()
    const owner = h.create()
    await owner.start({ operation: 'install', tools: [beta] })
    expect(h.downloads().sort()).toEqual([...piecesOf(h.first, 'tools/beta'), ...piecesOf(h.first, 'runtime/windows/rt'),
      ...piecesOf(h.first, 'tools/manifest.json')].sort())
    expect(await readFile(join(h.options.onlineRoot, 'runtime/windows/rt/rt.dll'), 'utf8')).toBe('runtime one')
    await expect(readdir(join(h.options.onlineRoot, 'tools/alpha'))).rejects.toMatchObject({ code: 'ENOENT' })
    expect((await readdir(h.options.cacheRoot)).filter(name => name.endsWith('.tar.gz'))).toEqual([])
    expect(owner.status()).toMatchObject({ phase: 'complete', operation: 'install', tools: [beta] })
    h.transport.mockClear()
    await owner.start({ operation: 'install', tools: [beta] })
    expect(h.downloads()).toEqual([])
    const library = new NativeToolsLibrary({ installRoot: h.root, userData: h.root, start: vi.fn(), inventory: () => owner.inventory() })
    expect((await library.listTools()).tools).toMatchObject([{ id: 'alpha', status: 'available' }, { id: 'beta', status: 'ready', outdated: false }])
    const refused = await library.launchTool(alpha)
    expect(refused.ok).toBe(false)
    expect(refused.error).toContain('尚未下载')
  })

  it('removes a tool and the runtime no remaining tool uses without network access', async () => {
    const h = await fixture()
    const owner = h.create()
    await owner.start({ operation: 'install', tools: [alpha, beta] })
    h.transport.mockClear()
    await owner.start({ operation: 'remove', tools: [beta] })
    expect(h.transport).not.toHaveBeenCalled()
    await expect(readdir(join(h.options.onlineRoot, 'runtime/windows/rt'))).rejects.toMatchObject({ code: 'ENOENT' })
    await expect(readdir(join(h.options.onlineRoot, 'tools/beta'))).rejects.toMatchObject({ code: 'ENOENT' })
    expect(await readFile(join(h.options.onlineRoot, 'tools/alpha/app.exe'), 'utf8')).toBe('alpha one')
    expect(Object.fromEntries((await owner.inventory()).states)).toMatchObject({ alpha: { installed: true }, beta: { installed: false } })
  })

  it('updates only the changed tool from a newer signed channel', async () => {
    const h = await fixture()
    const owner = h.create()
    await owner.start({ operation: 'install', tools: [alpha, beta] })
    expect(await owner.checkUpdates()).toMatchObject({ phase: 'current', version: '1.0.1' })
    h.publishRevision(h.second)
    expect(await owner.checkUpdates()).toMatchObject({ phase: 'available', version: '1.0.2' })
    const inventory = await owner.inventory()
    expect(inventory.catalogOutdated).toBe(true)
    const bytes = (unit: string): number => h.second.metadata.archives.find(archive => archive.unit === unit)?.bytes ?? 0
    expect(inventory.updateBytes).toBe(bytes('tools/alpha') + bytes('tools/manifest.json'))
    expect(Object.fromEntries(inventory.states)).toMatchObject({ alpha: { outdated: true }, beta: { outdated: false, downloadBytes: 0 } })
    h.transport.mockClear()
    await owner.start({ operation: 'update' })
    expect(h.downloads().sort()).toEqual([...piecesOf(h.second, 'tools/alpha'), ...piecesOf(h.second, 'tools/manifest.json')].sort())
    expect(await readFile(join(h.options.onlineRoot, 'tools/alpha/app.exe'), 'utf8')).toBe('alpha two')
    // A restarted carrier keeps the newer cached revision instead of its own older channel.
    expect((await h.create().inventory()).catalogOutdated).toBe(false)
  })

  it('ignores an older publisher channel and rejects a conflicting revision', async () => {
    const h = await fixture()
    const owner = h.create()
    h.publishRevision(h.second)
    await owner.checkUpdates()
    h.publishRevision(h.first)
    expect(await owner.checkUpdates()).toMatchObject({ phase: 'current', version: '1.0.2' })
    h.publishRevision({ ...h.first, revision: 2 })
    expect(await owner.checkUpdates()).toMatchObject({ phase: 'error', error: '工具更新清单冲突' })
  })

  it('repairs damaged files by downloading only the affected tool', async () => {
    const h = await fixture()
    const owner = h.create()
    await owner.start({ operation: 'install', tools: [alpha, beta] })
    await writeFile(join(h.options.onlineRoot, 'tools/beta/app.exe'), 'beta 0ne')
    h.transport.mockClear()
    await owner.start({ operation: 'repair' })
    expect(h.downloads().sort()).toEqual(piecesOf(h.first, 'tools/beta').sort())
    expect(await readFile(join(h.options.onlineRoot, 'tools/beta/app.exe'), 'utf8')).toBe('beta one')
  })

  it('cancels a blocked request and resumes the partial piece with Range', async () => {
    const h = await fixture()
    const owner = h.create()
    const entered = Promise.withResolvers<undefined>()
    const first = piecesOf(h.first, 'tools/alpha')[0] ?? ''
    const original = h.transport.getMockImplementation()
    h.transport.mockImplementation(async (input, init) => {
      if (href(input) !== first) return original?.(input, init) ?? new Response(null, { status: 500 })
      entered.resolve(undefined)
      return await new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => { reject(new DOMException('Cancelled', 'AbortError')) }, { once: true })
      })
    })
    const running = owner.start({ operation: 'install', tools: [alpha] })
    await entered.promise
    await owner.cancel()
    await running
    expect(owner.status()).toMatchObject({ phase: 'cancelled', operation: 'install' })
    const archive = h.first.metadata.archives.find(entry => entry.unit === 'tools/alpha')
    const bytes = h.first.served.get(first) ?? Buffer.alloc(0)
    await writeFile(join(h.options.cacheRoot, `${archive?.file ?? ''}.partial`), bytes.subarray(0, 5))
    h.transport.mockImplementation(original ?? (async () => new Response(null, { status: 500 })))
    h.transport.mockClear()
    await owner.start({ operation: 'install', tools: [alpha] })
    const resumed = h.transport.mock.calls.find(([input]) => href(input) === first)?.[1]
    expect(new Headers(resumed?.headers).get('range')).toBe('bytes=5-')
    expect(await readFile(join(h.options.onlineRoot, 'tools/alpha/app.exe'), 'utf8')).toBe('alpha one')
  })

  it('rejects changed bytes and keeps the installed tools', async () => {
    const h = await fixture()
    const owner = h.create()
    const original = h.transport.getMockImplementation()
    h.transport.mockImplementationOnce(async input => new Response(new Uint8Array(h.first.served.get(href(input))?.length ?? 1).fill(1)))
    await expect(owner.start({ operation: 'install', tools: [alpha] })).rejects.toThrow('校验失败')
    expect(owner.status()).toMatchObject({ phase: 'error' })
    h.transport.mockImplementation(original ?? (async () => new Response(null, { status: 500 })))
    await owner.start({ operation: 'install', tools: [alpha] })
    expect(Object.fromEntries((await owner.inventory()).states)).toMatchObject({ alpha: { installed: true } })
  })

  it('updates tools installed in the carrier directory in place', async () => {
    const h = await fixture()
    const legacy = join(h.root, 'carrier')
    await mkdir(legacy)
    const metadataPath = join(h.root, 'first-metadata.json')
    await writeFile(metadataPath, JSON.stringify(h.first.metadata))
    const media = join(h.root, 'first-media')
    await mkdir(media)
    for (const archive of h.first.source.archives) {
      const parts = archive.pieces.map(piece => h.first.served.get(baseUrl + piece.file) ?? Buffer.alloc(0))
      await writeFile(join(media, archive.file), Buffer.concat(parts))
    }
    await h.options.install?.({ installRoot: legacy, mediaDirectory: media, metadataPath })
    const owner = h.create({ legacyRoot: legacy, legacyLocked: false })
    expect((await owner.inventory()).root).toBe(legacy)
    h.publishRevision(h.second)
    await owner.checkUpdates()
    await owner.start({ operation: 'update' })
    expect(await readFile(join(legacy, 'tools/alpha/app.exe'), 'utf8')).toBe('alpha two')
    await expect(readdir(h.options.onlineRoot)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('prunes program files from earlier backups and removes download files no archive uses', async () => {
    const h = await fixture()
    const owner = h.create()
    await owner.start({ operation: 'install', tools: [alpha] })
    const backup = join(h.options.onlineRoot, '.rainy-toolpack/backups', randomUUID(), 'tools/alpha')
    await mkdir(join(backup, 'saves'), { recursive: true })
    await writeFile(join(backup, 'app.exe'), 'alpha one')
    await writeFile(join(backup, 'saves/one.sav'), 'personal')
    await writeFile(join(h.options.cacheRoot, 'native-tools-0123456789abcdef.tar.gz.001'), 'stale volume')
    await writeFile(join(h.options.cacheRoot, 'channel.signed.json'), '{}')
    expect(await owner.maintain()).toBe('alpha one'.length)
    expect(await readdir(backup)).toEqual(['saves'])
    expect((await readdir(h.options.cacheRoot)).sort()).toEqual([`metadata-${h.first.metadata.id}.json`])
  })
})
