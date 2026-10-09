/** Optional components: pinned downloads, verified unpacking, removal and cleanup. */
import { createHash } from 'node:crypto'
import { mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, relative } from 'node:path'
import { c } from 'tar'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { OptionalModules } from '../src/optional-modules.ts'
import { RETIRED_RESOURCES, removeRetiredResources } from '../src/retired-resources.ts'

const roots: string[] = []
afterEach(async () => {
  for (const root of roots.splice(0)) {
    if (relative(tmpdir(), root).startsWith('..')) throw new Error('Test directory escaped temporary storage')
    await rm(root, { recursive: true, force: true })
  }
})
const hash = (value: Buffer): string => createHash('sha256').update(value).digest('hex')
const baseUrl = 'https://github.com/RainyMarks/RainyAgent/releases/download/v1.0.6-resources/'
const href = (input: RequestInfo | URL): string => typeof input === 'string' ? input : input instanceof URL ? input.href : input.url

async function archive(root: string, name: string, files: Record<string, string>): Promise<Buffer> {
  const source = join(root, `${name}-source`)
  for (const [path, value] of Object.entries(files)) {
    await mkdir(join(source, path, '..'), { recursive: true })
    await writeFile(join(source, path), value)
  }
  const chunks: Buffer[] = []
  for await (const chunk of c({ cwd: source, gzip: true, portable: true }, Object.keys(files))) chunks.push(Buffer.from(chunk))
  return Buffer.concat(chunks)
}

async function fixture() {
  const root = await mkdtemp(join(tmpdir(), 'rainy-modules-'))
  roots.push(root)
  const php = await archive(root, 'php', { 'php.exe': 'php program', 'ext/php_curl.dll': 'curl extension' })
  const strata = await archive(root, 'strata', { 'engine/strata.exe': 'engine' })
  const runtime = Buffer.from('linux runtime archive')
  const served = new Map<string, Buffer>()
  const entry = (id: string, kind: string, file: string, bytes: Buffer, parts: Buffer[]) => {
    const pieces = parts.map((part, index) => {
      const piece = { file: `rainy-${hash(Buffer.from(file)).slice(0, 20)}.${String(index + 1).padStart(3, '0')}`, bytes: part.length, sha256: hash(part) }
      served.set(baseUrl + piece.file, part)
      return piece
    })
    return { id, kind, file, bytes: bytes.length, sha256: hash(bytes), unpackedBytes: 42, baseUrl, pieces }
  }
  const descriptorPath = join(root, 'optional-modules.json')
  await writeFile(descriptorPath, JSON.stringify({ version: 1, modules: [
    entry('strata', 'directory', 'strata-runtime.tar.gz', strata, [strata]),
    entry('php', 'directory', 'php.tar.gz', php, [php.subarray(0, 20), php.subarray(20)]),
    entry('linux-runtime', 'file', 'linux-runtime.tar.gz', runtime, [runtime]),
  ] }))
  const transport = vi.fn<typeof fetch>(async (input) => {
    const value = served.get(href(input))
    if (value === undefined) throw new Error(`Unexpected download URL ${href(input)}`)
    return new Response(new Uint8Array(value))
  })
  const publish = vi.fn()
  const modules = new OptionalModules({ descriptorPath, root: join(root, 'modules'), fetch: transport, publish })
  return { root, modules, transport, publish, served }
}

describe('optional components', () => {
  it('lists the user-facing components without network access', async () => {
    const h = await fixture()
    const status = await h.modules.status()
    expect(status.map(module => [module.id, module.installed, module.unpackedBytes])).toEqual([['strata', false, 42], ['php', false, 42]])
    expect(status.every(module => module.downloadBytes > 0)).toBe(true)
    expect(h.transport).not.toHaveBeenCalled()
  })

  it('joins verified pieces, unpacks the component and needs no network afterwards', async () => {
    const h = await fixture()
    await h.modules.install('php')
    const path = await h.modules.path('php')
    expect(await readFile(join(path, 'ext/php_curl.dll'), 'utf8')).toBe('curl extension')
    expect(await h.modules.installed('php')).toBe(true)
    expect(h.publish).toHaveBeenLastCalledWith(expect.objectContaining({ phase: 'complete', module: 'php' }))
    expect(await readdir(join(h.root, 'modules', '.downloads'))).toEqual([])
    h.transport.mockClear()
    await h.modules.install('php')
    expect(h.transport).not.toHaveBeenCalled()
    await h.modules.remove('php')
    expect(await h.modules.installed('php')).toBe(false)
    expect((await readdir(join(h.root, 'modules'))).filter(name => !name.startsWith('.'))).toEqual([])
  })

  it('keeps a file component as an archive and reports progress to the starting carrier', async () => {
    const h = await fixture()
    const progress = vi.fn()
    await h.modules.install('linux-runtime', progress)
    expect(await readFile(await h.modules.path('linux-runtime'), 'utf8')).toBe('linux runtime archive')
    expect(progress).toHaveBeenLastCalledWith(21, 21)
    // The carrier owns this download's feedback; the settings page never sees it.
    expect(h.publish).not.toHaveBeenCalled()
    // Once WSL has unpacked it, the archive is removed without notifying the settings page either.
    await h.modules.remove('linux-runtime')
    expect(await h.modules.installed('linux-runtime')).toBe(false)
    expect(h.publish).not.toHaveBeenCalled()
  })

  it('rejects changed bytes, publishes the error, and succeeds on retry', async () => {
    const h = await fixture()
    const original = h.transport.getMockImplementation()
    h.transport.mockImplementationOnce(async () => new Response(new Uint8Array(20).fill(7)))
    await expect(h.modules.install('php')).rejects.toThrow('校验失败')
    expect(h.publish).toHaveBeenLastCalledWith(expect.objectContaining({ phase: 'error', module: 'php' }))
    expect(await h.modules.installed('php')).toBe(false)
    h.transport.mockImplementation(original ?? (async () => new Response(null, { status: 500 })))
    await h.modules.install('php')
    expect(await h.modules.installed('php')).toBe(true)
  })

  it('stops on cancellation and removes interrupted copies and unpinned downloads later', async () => {
    const h = await fixture()
    const entered = Promise.withResolvers<undefined>()
    h.transport.mockImplementationOnce((_input, init) => new Promise<Response>((_resolve, reject) => {
      entered.resolve(undefined)
      init?.signal?.addEventListener('abort', () => { reject(new DOMException('Cancelled', 'AbortError')) }, { once: true })
    }))
    const running = h.modules.install('strata')
    await entered.promise
    await h.modules.cancel()
    await running
    expect(h.modules.progress()).toMatchObject({ phase: 'cancelled', module: 'strata' })
    await mkdir(join(h.root, 'modules', '.staging-php-interrupted'), { recursive: true })
    await writeFile(join(h.root, 'modules', '.downloads', 'stale.tar.gz'), 'old')
    await h.modules.clean()
    expect(await readdir(join(h.root, 'modules'))).toEqual(['.downloads'])
    expect(await readdir(join(h.root, 'modules', '.downloads'))).toEqual([])
  })
})

describe('retired carrier resources', () => {
  it('deletes what earlier installers shipped and leaves current resources alone', async () => {
    const root = await mkdtemp(join(tmpdir(), 'rainy-retired-'))
    roots.push(root)
    await mkdir(join(root, 'strata-runtime/engine'), { recursive: true })
    await writeFile(join(root, 'strata-runtime/engine/strata.exe'), 'old engine')
    await writeFile(join(root, 'linux-runtime.tar.gz'), 'old runtime')
    await writeFile(join(root, 'optional-modules.json'), '{}')
    expect(await removeRetiredResources(root)).toEqual(['strata-runtime', 'linux-runtime.tar.gz'])
    expect(await readdir(root)).toEqual(['optional-modules.json'])
    expect(RETIRED_RESOURCES).not.toContain('optional-modules.json')
  })
})
