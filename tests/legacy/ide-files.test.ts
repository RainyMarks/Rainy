/** Real isolated filesystem evidence for human editor saves, containment and destructive confirmation. */
import { execFile } from 'node:child_process'
import { mkdir, mkdtemp, readFile, realpath, rename, rm, symlink, unlink, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { promisify } from 'node:util'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { brandString } from '@deepseek-ai/dsh-brand'
import { LocalFileSystem } from '@deepseek-ai/dsh-fs-local'
import type { Workspace, WorkspaceId } from '@deepseek-ai/dsh-workspace'
import { RainyIdeFiles, parseIdeFilesRequest, resolveIdeFilesConfig } from '../src/ide-files.ts'
import { ideFailure } from '../src/ide-files-core.ts'
import { searchIdeFiles } from '../src/ide-files-search.ts'
import Storage from '@deepseek-ai/dsh-storage'
import { DomainFacility } from '@deepseek-ai/dsh-storage-domain'
import { JsonStorageBackend } from '@deepseek-ai/dsh-storage-json'
import RainyProjectRoots from '../src/project-roots.ts'
import type { IdeRootId } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'

const cleanups: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
  vi.restoreAllMocks()
})

function project(id: WorkspaceId, path: string): Workspace {
  return { id, path, title: 'fixture', createdAt: '2026-01-01T00:00:00.000Z', updatedAt: '2026-01-01T00:00:00.000Z', sessionIds: [],
    setTitle: async () => {}, attachSession: async () => {}, insertSessionBefore: async () => {}, detachSession: async () => {}, status: async () => 'ok' }
}

async function fixture(config: Parameters<typeof resolveIdeFilesConfig>[0] = {}) {
  const temporary = await mkdtemp(join(tmpdir(), 'rainy-ide-files-'))
  cleanups.push(() => rm(temporary, { recursive: true, force: true }))
  const root = join(temporary, '中文 project')
  const outside = join(temporary, 'outside')
  await mkdir(root)
  await mkdir(outside)
  const ctx = new Context()
  cleanups.push(() => ctx.fiber.dispose())
  await ctx.plugin(LocalFileSystem, { cwd: root })
  const fs = ctx.fs as LocalFileSystem
  const workspaceId = brandString<WorkspaceId>('fixture-workspace')
  const workspace = project(workspaceId, await realpath(root))
  const records = new Map([[workspaceId, workspace]])
  const registry = { get: (id: WorkspaceId) => records.get(id), list: () => [...records.values()], create: vi.fn(async () => workspace) }
  ctx.provide('workspaceRegistry', registry)
  await ctx.plugin(Storage)
  const backend = new JsonStorageBackend(join(temporary, 'storage'))
  ctx.effect(() => () => backend.close())
  ctx.effect(() => ctx.storage.backend.register('json', backend))
  ctx.provide('storageDomain', new DomainFacility(ctx, { backend: 'json', routes: {} }))
  await ctx.plugin(RainyProjectRoots, { maxRoots: 16 })
  await vi.waitFor(() => { expect(ctx.get('rainyProjectRoots')?.get(workspaceId)).toHaveLength(1) })
  let now = 100
  const files = new RainyIdeFiles({ registry, roots: ctx.rainyProjectRoots, fs, config: resolveIdeFilesConfig(config), now: () => now })
  cleanups.push(() => files.close())
  return { temporary, root, outside, fs, files, registry, workspaceId, records, roots: ctx.rainyProjectRoots,
    advance: (milliseconds: number) => { now += milliseconds } }
}

describe('Rainy project directory mounts', () => {
  it('keeps independent roots in one project and saves same-named files to their own directory', async () => {
    const { root, outside, files, workspaceId, registry } = await fixture()
    await writeFile(join(root, 'main.py'), 'primary\n')
    await writeFile(join(outside, 'main.py'), 'attached\n')
    const workspace = await files.handle({ op: 'workspaces.attach', workspaceId, path: outside })
    expect(registry.list()).toHaveLength(1)
    const attached = workspace.roots?.find(entry => !entry.primary)
    expect(attached).toBeDefined()
    if (attached === undefined) throw new Error('Attached root was not returned')
    const original = await files.handle({ op: 'files.read', workspaceId, path: 'main.py', rootId: attached.rootId })
    expect(original.content).toBe('attached\n')
    await files.handle({ op: 'files.save', workspaceId, rootId: attached.rootId, path: 'main.py',
      content: 'saved attachment\n', expectedVersion: original.version })
    expect(await readFile(join(root, 'main.py'), 'utf8')).toBe('primary\n')
    expect(await readFile(join(outside, 'main.py'), 'utf8')).toBe('saved attachment\n')
    await files.handle({ op: 'workspaces.removeRoot', workspaceId, rootId: attached.rootId })
    expect(await readFile(join(outside, 'main.py'), 'utf8')).toBe('saved attachment\n')
    await expect(files.handle({ op: 'files.read', workspaceId, rootId: attached.rootId, path: 'main.py' })).rejects.toMatchObject({ code: 'workspace-not-found' })
  })

  it('reuses exact duplicates and rejects overlapping mounts without expanding the project', async () => {
    const { root, outside, files, workspaceId } = await fixture()
    const first = await files.handle({ op: 'workspaces.attach', workspaceId, path: outside })
    expect(await files.handle({ op: 'workspaces.attach', workspaceId, path: outside })).toEqual(first)
    await mkdir(join(root, 'nested'))
    await expect(files.handle({ op: 'workspaces.attach', workspaceId, path: join(root, 'nested') })).rejects.toMatchObject({ code: 'root-overlap' })
    expect((await files.handle({ op: 'workspaces.list' }))[0]?.roots).toHaveLength(2)
  })

  it('preserves imported root identities and rejects conflicting restored mappings', async () => {
    const { root, outside, roots, workspaceId, temporary } = await fixture()
    const rootId = brandString<IdeRootId>('carrier-root')
    const imported = await roots.importRoot(workspaceId, { rootId, path: outside, title: 'Shared source' })
    expect(imported[1]).toMatchObject({ rootId, path: await realpath(outside), title: 'Shared source' })
    expect(roots.forSessionCwd(await realpath(root))).toEqual(imported)
    expect(roots.forSessionCwd(outside)).toEqual([])
    await expect(roots.importRoot(workspaceId, { rootId: brandString<IdeRootId>('another-root'), path: outside, title: 'Other' }))
      .rejects.toMatchObject({ code: 'invalid-request' })
    const moved = join(temporary, 'unavailable')
    await rename(outside, moved)
    expect(roots.get(workspaceId)).toEqual(imported)
    await expect(roots.resolveRoot(workspaceId, rootId)).rejects.toMatchObject({ code: 'workspace-unavailable' })
  })
})

describe('Rainy IDE text files', () => {
  it('reads the complete UTF-8 file and saves BOM, CRLF and the requested final newline exactly', async () => {
    const { root, files, workspaceId } = await fixture()
    const content = '\uFEFF第一行\r\nsecond\r\n'
    await writeFile(join(root, 'hello.py'), content)
    const before = await files.handle({ op: 'files.read', workspaceId, path: 'hello.py' })
    expect(before).toMatchObject({ content: '第一行\r\nsecond\r\n', bom: true, eol: 'crlf', readOnlyReason: null })
    const saved = await files.handle({ op: 'files.save', workspaceId, path: 'hello.py', expectedVersion: before.version, content: '第一行\nchanged' })
    expect(await readFile(join(root, 'hello.py'), 'utf8')).toBe('\uFEFF第一行\r\nchanged')
    expect(saved.version).not.toBe(before.version)
    expect(saved.content).toBe('第一行\r\nchanged')
    const unchanged = await files.handle({ op: 'files.save', workspaceId, path: 'hello.py', expectedVersion: saved.version, content: saved.content ?? '' })
    expect(await readFile(join(root, 'hello.py'), 'utf8')).toBe('\uFEFF第一行\r\nchanged')
    expect(unchanged.readOnlyReason).toBeNull()
  })

  it('preserves mixed line endings on an unchanged save', async () => {
    const { root, files, workspaceId } = await fixture()
    await writeFile(join(root, 'mixed.txt'), 'a\r\nb\nc')
    const read = await files.handle({ op: 'files.read', workspaceId, path: 'mixed.txt' })
    expect(read.eol).toBe('mixed')
    await files.handle({ op: 'files.save', workspaceId, path: 'mixed.txt', expectedVersion: read.version, content: read.content ?? '' })
    expect(await readFile(join(root, 'mixed.txt'), 'utf8')).toBe('a\r\nb\nc')
  })

  it('keeps binary, unsupported encoding and over-budget contents out of editable documents', async () => {
    const { root, files, workspaceId } = await fixture({ maxTextBytes: 12 })
    await writeFile(join(root, 'binary'), Buffer.from([0, 1, 2]))
    await writeFile(join(root, 'utf16'), Buffer.from([0xff, 0xfe, 0x61, 0]))
    await writeFile(join(root, 'large'), '1234567890123')
    for (const [path, readOnlyReason] of [['binary', 'binary'], ['utf16', 'unsupported-encoding'], ['large', 'too-large']] as const) {
      const document = await files.handle({ op: 'files.read', workspaceId, path })
      expect(document).toMatchObject({ content: null, readOnlyReason })
      await expect(files.handle({ op: 'files.save', workspaceId, path, expectedVersion: document.version, content: 'replace' })).rejects.toMatchObject({ code: 'read-only' })
    }
    expect(await readFile(join(root, 'binary'))).toEqual(Buffer.from([0, 1, 2]))
    await expect(files.handle({ op: 'files.create', workspaceId, path: 'over', content: '1234567890123' })).rejects.toMatchObject({ code: 'too-large' })
  })

  it('previews a bounded UTF-8 prefix without exposing incomplete code points or editable source', async () => {
    const { root, fs, files, workspaceId } = await fixture({ maxTextBytes: 4, maxPreviewBytes: 5 })
    await writeFile(join(root, 'large.txt'), 'a中中文本')
    const whole = vi.spyOn(fs, 'readBytes')
    const range = vi.spyOn(fs, 'readByteRange')
    const document = await files.handle({ op: 'files.read', workspaceId, path: 'large.txt' })
    expect(document).toMatchObject({ content: null, bytes: 13, readOnlyReason: 'too-large',
      preview: { kind: 'utf8', text: 'a中', bytesRead: 5, truncated: true } })
    expect(whole).not.toHaveBeenCalled()
    expect(range).toHaveBeenCalledWith(expect.anything(), { offset: 0, length: 5 }, undefined)
  })

  it('previews binary bytes and malformed UTF-8 as bounded hex and ASCII', async () => {
    const { root, files, workspaceId } = await fixture({ maxTextBytes: 4, maxPreviewBytes: 5, maxHexPreviewBytes: 2 })
    await writeFile(join(root, 'binary.bin'), Buffer.from([0, 65, 0, 66, 0, 67]))
    await writeFile(join(root, 'bad.txt'), Buffer.from([0x61, 0xff, 0x62]))
    const binary = await files.handle({ op: 'files.read', workspaceId, path: 'binary.bin' })
    expect(binary).toMatchObject({ content: null, bytes: 6, readOnlyReason: 'binary',
      preview: { kind: 'hex', bytesRead: 2, truncated: true } })
    expect(binary.preview?.text).toContain('00 41')
    expect(binary.preview?.text).toContain('|.A|')
    const unsupported = await files.handle({ op: 'files.read', workspaceId, path: 'bad.txt' })
    expect(unsupported).toMatchObject({ content: null, readOnlyReason: 'unsupported-encoding',
      preview: { kind: 'hex', bytesRead: 2, truncated: true } })
    expect(unsupported.preview?.text).toContain('61 ff')
  })

  it('rejects a prefix when the file changes while its preview is being read', async () => {
    const { root, fs, files, workspaceId } = await fixture({ maxTextBytes: 4, maxPreviewBytes: 5 })
    await writeFile(join(root, 'large.txt'), 'before preview')
    const range = fs.readByteRange.bind(fs)
    vi.spyOn(fs, 'readByteRange').mockImplementationOnce(async (...args) => {
      const bytes = await range(...args)
      await writeFile(join(root, 'large.txt'), 'changed preview contents')
      return bytes
    })
    await expect(files.handle({ op: 'files.read', workspaceId, path: 'large.txt' })).rejects.toMatchObject({ code: 'version-conflict' })
  })

  it('refuses stale and deleted files without overwriting a newer disk version', async () => {
    const { root, files, workspaceId } = await fixture()
    await writeFile(join(root, 'main.py'), 'first')
    const opened = await files.handle({ op: 'files.read', workspaceId, path: 'main.py' })
    await writeFile(join(root, 'main.py'), 'external content')
    await expect(files.handle({ op: 'files.save', workspaceId, path: 'main.py', expectedVersion: opened.version, content: 'stale' })).rejects.toMatchObject({ code: 'version-conflict' })
    expect(await readFile(join(root, 'main.py'), 'utf8')).toBe('external content')
    await unlink(join(root, 'main.py'))
    await expect(files.handle({ op: 'files.save', workspaceId, path: 'main.py', expectedVersion: opened.version, content: 'resurrection' })).rejects.toMatchObject({ code: 'version-conflict', currentVersion: null })
  })

  it('serializes overlapping saves and drains the admitted winner on close', async () => {
    const { files, fs, root, workspaceId } = await fixture()
    await writeFile(join(root, 'main.py'), 'base')
    const opened = await files.handle({ op: 'files.read', workspaceId, path: 'main.py' })
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    const write = fs.writeText.bind(fs)
    vi.spyOn(fs, 'writeText').mockImplementationOnce(async (...args) => { entered.resolve(undefined); await release.promise; return write(...args) })
    const first = files.handle({ op: 'files.save', workspaceId, path: 'main.py', expectedVersion: opened.version, content: 'first' })
    await entered.promise
    const second = files.handle({ op: 'files.save', workspaceId, path: 'main.py', expectedVersion: opened.version, content: 'second' })
    const closing = files.close()
    await expect(files.handle({ op: 'workspaces.list' })).rejects.toMatchObject({ code: 'closed' })
    release.resolve(undefined)
    await expect(first).resolves.toMatchObject({ content: 'first' })
    await expect(second).rejects.toMatchObject({ code: 'version-conflict' })
    await closing
    expect(await readFile(join(root, 'main.py'), 'utf8')).toBe('first')
  })

  it('detects a file changed while the full read is in flight', async () => {
    const { root, fs, files, workspaceId } = await fixture()
    await writeFile(join(root, 'read.py'), 'before')
    const read = fs.readBytes.bind(fs)
    vi.spyOn(fs, 'readBytes').mockImplementationOnce(async (...args) => {
      const bytes = await read(...args)
      await writeFile(join(root, 'read.py'), 'after change')
      return bytes
    })
    await expect(files.handle({ op: 'files.read', workspaceId, path: 'read.py' })).rejects.toMatchObject({ code: 'version-conflict' })
  })

  it('preserves the original file after atomic staging fails and permits a later save', async () => {
    const { root, fs, files, workspaceId } = await fixture()
    await writeFile(join(root, 'atomic.py'), 'original')
    const opened = await files.handle({ op: 'files.read', workspaceId, path: 'atomic.py' })
    fs.internals.inspectTemp = () => Promise.reject(new Error('fixture staging failure'))
    await expect(files.handle({ op: 'files.save', workspaceId, path: 'atomic.py', expectedVersion: opened.version, content: 'failed' })).rejects.toThrow('fixture staging failure')
    expect(await readFile(join(root, 'atomic.py'), 'utf8')).toBe('original')
    fs.internals = {}
    await files.handle({ op: 'files.save', workspaceId, path: 'atomic.py', expectedVersion: opened.version, content: 'saved' })
    expect(await readFile(join(root, 'atomic.py'), 'utf8')).toBe('saved')
  })

  it('creates and renames without an Agent and leaves existing destinations unchanged', async () => {
    const { files, workspaceId, root, registry } = await fixture()
    expect(await files.handle({ op: 'workspaces.list' })).toEqual([{ workspaceId, path: await realpath(root), title: 'fixture',
      roots: [{ rootId: 'primary', path: await realpath(root), title: 'fixture', primary: true }] }])
    await files.handle({ op: 'workspaces.open', path: root })
    expect(registry.create).toHaveBeenCalledWith(root)
    await files.handle({ op: 'files.mkdir', workspaceId, path: 'src' })
    const document = await files.handle({ op: 'files.create', workspaceId, path: 'src/new.py', content: '' })
    const moved = await files.handle({ op: 'files.rename', workspaceId, path: 'src/new.py', destination: 'src/main.py', expectedVersion: document.version })
    expect(moved).toMatchObject({ path: 'src/main.py', kind: 'file' })
    await expect(files.handle({ op: 'files.create', workspaceId, path: 'src/main.py', content: 'clobber' })).rejects.toMatchObject({ code: 'already-exists' })
    await files.handle({ op: 'files.create', workspaceId, path: 'other.py', content: 'keep' })
    await expect(files.handle({ op: 'files.rename', workspaceId, path: 'src/main.py', destination: 'other.py', expectedVersion: moved.version })).rejects.toMatchObject({ code: 'already-exists' })
    expect(await readFile(join(root, 'other.py'), 'utf8')).toBe('keep')
  })

  it('refuses a competing destination created after the preliminary absence check', async () => {
    const { root, fs, files, workspaceId } = await fixture()
    await writeFile(join(root, 'source.py'), 'source')
    const opened = await files.handle({ op: 'files.read', workspaceId, path: 'source.py' })
    const inspect = fs.lstat.bind(fs)
    vi.spyOn(fs, 'lstat').mockImplementation(async (path, ...args) => {
      const result = await inspect(path, ...args)
      if (path === join(root, 'destination.py') && result === undefined) await writeFile(path, 'competitor')
      return result
    })
    await expect(files.handle({ op: 'files.rename', workspaceId, path: 'source.py', destination: 'destination.py', expectedVersion: opened.version })).rejects.toMatchObject({ code: 'EEXIST' })
    expect(await readFile(join(root, 'source.py'), 'utf8')).toBe('source')
    expect(await readFile(join(root, 'destination.py'), 'utf8')).toBe('competitor')
  })

  it('renames a directory and its children with the same guarded operation', async () => {
    const { root, files, workspaceId } = await fixture()
    await mkdir(join(root, 'source'))
    await writeFile(join(root, 'source/main.py'), 'print(1)')
    const listing = await files.handle({ op: 'files.list', workspaceId, path: '' })
    const directory = listing.entries.find(entry => entry.path === 'source')
    if (directory === undefined) throw new Error('Fixture directory did not appear.')
    await files.handle({ op: 'files.rename', workspaceId, path: 'source', destination: 'renamed', expectedVersion: directory.version })
    expect(await readFile(join(root, 'renamed/main.py'), 'utf8')).toBe('print(1)')
    await expect(readFile(join(root, 'source/main.py'))).rejects.toMatchObject({ code: 'ENOENT' })
  })
})

describe('Rainy IDE filename discovery', () => {
  it('finds unexpanded files by path tokens while omitting excluded directories and directory links', async () => {
    const { root, outside, fs, files, workspaceId } = await fixture()
    await mkdir(join(root, 'src'))
    await mkdir(join(root, 'node_modules'))
    await writeFile(join(root, 'src/Main.py'), 'print(1)')
    await writeFile(join(root, 'src/other.ts'), 'export {}')
    await writeFile(join(root, 'node_modules/main.py'), 'dependency')
    await writeFile(join(outside, 'escape.py'), 'outside')
    await symlink(outside, join(root, 'outside-link'), process.platform === 'win32' ? 'junction' : 'dir')
    await symlink(join(root, 'src'), join(root, 'alias'), process.platform === 'win32' ? 'junction' : 'dir')
    const whole = vi.spyOn(fs, 'readBytes')
    const range = vi.spyOn(fs, 'readByteRange')
    const observe = vi.spyOn(fs, 'lstat')
    expect(await files.handle({ op: 'files.search', workspaceId, query: 'SRC .PY' })).toEqual({ paths: ['src/Main.py'], truncated: false })
    expect(await files.handle({ op: 'files.search', workspaceId, query: '' })).toEqual({ paths: ['src/Main.py', 'src/other.ts'], truncated: false })
    expect(whole).not.toHaveBeenCalled()
    expect(range).not.toHaveBeenCalled()
    expect(observe).not.toHaveBeenCalled()
  })

  it('bounds results and scanned directory entries and honors configured exclusions', async () => {
    const { root, files, workspaceId } = await fixture({ searchResultLimit: 2, searchMaxEntries: 2, searchExcludedDirectories: ['skip'] })
    for (const name of ['a.py', 'b.py', 'c.py']) await writeFile(join(root, name), '')
    const result = await files.handle({ op: 'files.search', workspaceId, query: '.py', limit: 999 })
    expect(result.paths).toHaveLength(2)
    expect(result.truncated).toBe(true)
    expect(await files.handle({ op: 'files.search', workspaceId, query: 'no-match' })).toEqual({ paths: [], truncated: true })
    const explicit = await files.handle({ op: 'files.search', workspaceId, query: '.py', limit: 1 })
    expect(explicit.paths).toHaveLength(1)
    expect(explicit.truncated).toBe(true)
    await mkdir(join(root, 'skip'))
    await writeFile(join(root, 'skip/ignored'), '')
    expect(await searchIdeFiles(root, 'ignored', undefined, resolveIdeFilesConfig({ searchExcludedDirectories: ['skip'] })))
      .toEqual({ paths: [], truncated: false })
  })

  it('returns incomplete results at the deadline and rejects caller cancellation', async () => {
    const { root } = await fixture()
    await writeFile(join(root, 'file.py'), '')
    let tick = 0
    expect(await searchIdeFiles(root, '', undefined, resolveIdeFilesConfig({ searchTimeoutMs: 3 }), undefined, () => tick++))
      .toEqual({ paths: [], truncated: true })
    const controller = new AbortController()
    const clock = () => { if (++tick > 5) controller.abort(); return tick }
    await expect(searchIdeFiles(root, '', undefined, resolveIdeFilesConfig(), controller.signal, clock)).rejects.toThrow()
    for (const fields of [{ query: 'x', limit: 0 }, { query: 'x'.repeat(513) }, { query: 'x', command: 'ignored' }]) {
      expect(() => parseIdeFilesRequest({ op: 'files.search', workspaceId: 'fixture', ...fields })).toThrow()
    }
  })
})

describe('Rainy IDE path and deletion ownership', () => {
  it('refuses a workspace root replaced with a link to a different directory', async () => {
    const { root, outside, files, workspaceId } = await fixture()
    await writeFile(join(outside, 'keep'), 'outside')
    await rename(root, `${root}.original`)
    await symlink(outside, root, process.platform === 'win32' ? 'junction' : 'dir')
    await expect(files.handle({ op: 'files.read', workspaceId, path: 'keep' })).rejects.toMatchObject({ code: 'workspace-unavailable' })
    expect(await readFile(join(outside, 'keep'), 'utf8')).toBe('outside')
  })

  it('bounds directory scans and deletion previews before publishing any mutation', async () => {
    const { root, files, workspaceId } = await fixture({ maxDirectoryEntries: 2, maxDeleteEntries: 2 })
    await mkdir(join(root, 'tree'))
    for (const name of ['a', 'b', 'c']) await writeFile(join(root, 'tree', name), name)
    await expect(files.handle({ op: 'files.list', workspaceId, path: 'tree' })).rejects.toMatchObject({ code: 'too-large' })
    await expect(files.handle({ op: 'files.deletePreview', workspaceId, path: 'tree' })).rejects.toMatchObject({ code: 'too-large' })
    expect(await readFile(join(root, 'tree/a'), 'utf8')).toBe('a')
  })

  it('rejects relative escapes and links into an outside directory for reads and writes', async () => {
    const { root, outside, files, workspaceId } = await fixture()
    await writeFile(join(outside, 'secret.txt'), 'outside')
    await symlink(outside, join(root, 'outside-link'), process.platform === 'win32' ? 'junction' : 'dir')
    const listing = await files.handle({ op: 'files.list', workspaceId, path: '' })
    expect(listing.entries.find(entry => entry.name === 'outside-link')).toMatchObject({ kind: 'symlink', outsideWorkspace: true })
    for (const path of ['../outside/secret.txt', '/absolute', 'C:/absolute', 'a\\..\\b']) {
      await expect(files.handle({ op: 'files.read', workspaceId, path })).rejects.toMatchObject({ code: 'invalid-path' })
    }
    await expect(files.handle({ op: 'files.read', workspaceId, path: 'outside-link/secret.txt' })).rejects.toMatchObject({ code: 'outside-workspace' })
    await expect(files.handle({ op: 'files.create', workspaceId, path: 'outside-link/new.txt', content: 'escape' })).rejects.toMatchObject({ code: 'outside-workspace' })
    expect(await readFile(join(outside, 'secret.txt'), 'utf8')).toBe('outside')
  })

  it('uses a contained directory link for file editing while retaining the link', async () => {
    const { root, files, workspaceId } = await fixture()
    await mkdir(join(root, 'actual'))
    await writeFile(join(root, 'actual/file.txt'), 'before')
    await symlink(join(root, 'actual'), join(root, 'alias'), process.platform === 'win32' ? 'junction' : 'dir')
    const opened = await files.handle({ op: 'files.read', workspaceId, path: 'alias/file.txt' })
    await files.handle({ op: 'files.save', workspaceId, path: 'alias/file.txt', expectedVersion: opened.version, content: 'after' })
    expect(await readFile(join(root, 'actual/file.txt'), 'utf8')).toBe('after')
    expect(await realpath(join(root, 'alias'))).toBe(await realpath(join(root, 'actual')))
  })

  it('requires one-use confirmation and refuses a changed deletion tree', async () => {
    const { root, files, workspaceId } = await fixture()
    await mkdir(join(root, 'remove'))
    await expect(files.handle({ op: 'files.deletePreview', workspaceId, path: '' })).rejects.toMatchObject({ code: 'invalid-path' })
    await writeFile(join(root, 'remove/a'), 'original')
    const first = await files.handle({ op: 'files.deletePreview', workspaceId, path: 'remove' })
    expect(first.entries).toBe(2)
    await writeFile(join(root, 'remove/a'), 'changed')
    await expect(files.handle({ op: 'files.delete', workspaceId, path: 'remove', token: first.token })).rejects.toMatchObject({ code: 'version-conflict' })
    expect(await readFile(join(root, 'remove/a'), 'utf8')).toBe('changed')
    await expect(files.handle({ op: 'files.delete', workspaceId, path: 'remove', token: first.token })).rejects.toMatchObject({ code: 'confirmation-required' })
    const preview = await files.handle({ op: 'files.deletePreview', workspaceId, path: 'remove' })
    await expect(files.handle({ op: 'files.delete', workspaceId, path: 'remove', token: preview.token })).resolves.toEqual({ path: 'remove', deleted: true })
    await expect(readFile(join(root, 'remove/a'))).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it('unlinks an outside junction without deleting any referenced contents and expires old confirmations', async () => {
    const { root, outside, files, workspaceId, advance } = await fixture({ deletePreviewLifetimeMs: 50 })
    await writeFile(join(outside, 'keep'), 'keep')
    await symlink(outside, join(root, 'link'), process.platform === 'win32' ? 'junction' : 'dir')
    const expired = await files.handle({ op: 'files.deletePreview', workspaceId, path: 'link' })
    advance(50)
    await expect(files.handle({ op: 'files.delete', workspaceId, path: 'link', token: expired.token })).rejects.toMatchObject({ code: 'confirmation-required' })
    const preview = await files.handle({ op: 'files.deletePreview', workspaceId, path: 'link' })
    expect(preview.entries).toBe(1)
    await files.handle({ op: 'files.delete', workspaceId, path: 'link', token: preview.token })
    expect(await readFile(join(outside, 'keep'), 'utf8')).toBe('keep')
  })

  it('reports changed and removed open paths without modifying files', async () => {
    const { root, files, workspaceId } = await fixture()
    await writeFile(join(root, 'a'), 'before')
    const opened = await files.handle({ op: 'files.read', workspaceId, path: 'a' })
    await writeFile(join(root, 'a'), 'after')
    const changes = await files.handle({ op: 'files.changes', workspaceId, paths: ['a', 'missing', 'a', ''] })
    expect(changes).toHaveLength(3)
    expect(changes[0]?.version).not.toBe(opened.version)
    expect(changes[1]).toEqual({ path: 'missing', version: null, kind: 'missing' })
    expect(changes[2]).toMatchObject({ path: '', kind: 'directory' })
  })

  it('rejects cancelled mutations, deleted workspace identities and malformed JSON requests', async () => {
    const { files, workspaceId, records } = await fixture()
    const signal = AbortSignal.abort()
    await expect(files.handle({ op: 'files.create', workspaceId, path: 'cancelled', content: '' }, signal)).rejects.toThrow()
    records.delete(workspaceId)
    await expect(files.handle({ op: 'files.read', workspaceId, path: 'file' })).rejects.toMatchObject({ code: 'workspace-not-found' })
    expect(() => parseIdeFilesRequest({ op: 'files.save', workspaceId, path: 'x', content: 'bad', expectedVersion: 'v', bypass: true })).toThrow()
    expect(() => parseIdeFilesRequest({ op: 'files.delete', workspaceId, path: 'x' })).toThrow()
    expect(ideFailure(Object.assign(new Error('denied'), { code: 'EACCES' }))).toEqual({ code: 'permission-denied', message: 'denied' })
  })
})

describe('Rainy IDE Git comparison', () => {
  it('compares a nested workspace file, added file and deleted file against HEAD', async () => {
    const { root, files, workspaceId, temporary, records } = await fixture()
    const git = async (...args: string[]) => promisify(execFile)('git', ['-C', temporary, ...args], { windowsHide: true })
    await git('init', '-q')
    await writeFile(join(root, 'tracked.txt'), 'base\n')
    await git('add', '--all')
    await git('-c', 'user.name=IDE fixture', '-c', 'user.email=ide@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture')
    await writeFile(join(root, 'tracked.txt'), 'modified\n')
    expect(await files.handle({ op: 'files.diff', workspaceId, path: 'tracked.txt' })).toMatchObject({ base: 'base\n', current: 'modified\n', status: 'modified' })
    await writeFile(join(root, 'new.txt'), 'new')
    expect(await files.handle({ op: 'files.diff', workspaceId, path: 'new.txt' })).toMatchObject({ base: '', current: 'new', status: 'added' })
    await unlink(join(root, 'tracked.txt'))
    expect(await files.handle({ op: 'files.diff', workspaceId, path: 'tracked.txt' })).toMatchObject({ base: 'base\n', current: null, status: 'deleted' })
    expect(records.get(workspaceId)?.sessionIds).toEqual([])
  })

  it('treats a CRLF checkout of an LF blob as unchanged', async () => {
    const { root, files, workspaceId, temporary } = await fixture()
    const git = async (...args: string[]) => promisify(execFile)('git', ['-C', temporary, ...args], { windowsHide: true })
    await git('init', '-q')
    await writeFile(join(root, 'lines.txt'), 'one\ntwo\n')
    await git('-c', 'core.autocrlf=false', 'add', '--all')
    await git('-c', 'user.name=IDE fixture', '-c', 'user.email=ide@example.invalid', '-c', 'commit.gpgsign=false', 'commit', '-qm', 'fixture')
    await writeFile(join(root, 'lines.txt'), 'one\r\ntwo\r\n')
    expect(await files.handle({ op: 'files.diff', workspaceId, path: 'lines.txt' }))
      .toMatchObject({ base: 'one\r\ntwo\r\n', current: 'one\r\ntwo\r\n', status: 'unchanged' })
    await writeFile(join(root, 'lines.txt'), 'one\r\nthree\r\n')
    expect(await files.handle({ op: 'files.diff', workspaceId, path: 'lines.txt' })).toMatchObject({ status: 'modified' })
  })

  it('returns an explicit unavailable comparison outside Git', async () => {
    const { root, files, workspaceId } = await fixture()
    await writeFile(join(root, 'x'), 'text')
    expect(await files.handle({ op: 'files.diff', workspaceId, path: 'x' })).toMatchObject({ base: null, current: 'text', status: 'unavailable', reason: 'not-git' })
  })
})
