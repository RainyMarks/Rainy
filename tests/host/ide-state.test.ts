/** Atomic per-project recovery files, revision conflicts and the remembered project selection. */
import { mkdir, mkdtemp, readFile, rm, rmdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { IdeFileVersion, IdeRootId, IdeWorkspaceStateData, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { writeJson } from '../../src/host/files.ts'
import { ideStateFileName, parseIdeStateRequest, RainyIdeStateStore, resolveIdeStateConfig } from '../../src/host/ide/state.ts'
import { Projects } from '../../src/host/projects.ts'

const cleanups: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
  vi.restoreAllMocks()
})

async function fixture(config: Parameters<typeof resolveIdeStateConfig>[0] = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rainy-ide-state-'))
  cleanups.push(() => rm(root, { recursive: true, force: true }))
  const projects = new Projects(join(root, 'projects.json'))
  await projects.load()
  await mkdir(join(root, 'a'))
  await mkdir(join(root, 'b'))
  const workspaceId = (await projects.open(join(root, 'a'))).id
  const otherId = (await projects.open(join(root, 'b'))).id
  const directory = join(root, 'ide')
  const resolved = resolveIdeStateConfig(config)
  const writes = { gate: undefined as Promise<void> | undefined, entered: undefined as (() => void) | undefined }
  const write = async (path: string, value: unknown): Promise<void> => {
    if (writes.gate !== undefined) {
      const gate = writes.gate
      writes.gate = undefined
      writes.entered?.()
      await gate
    }
    await writeJson(path, value)
  }
  const open = (log?: (message: string) => void) => RainyIdeStateStore.open({ directory, projects, config: resolved, write, log })
  const store = await open()
  cleanups.push(() => store.close())
  const empty = (await store.get(workspaceId)).data
  const data = (text: string): IdeWorkspaceStateData => ({
    ...empty,
    tabs: [{ path: '中文/main.py', kind: 'file', cursor: { line: 2, column: 3 }, scroll: { top: 30, left: 0 } }],
    activePath: '中文/main.py', expandedPaths: ['中文'],
    buffers: [{ path: '中文/main.py', content: text, baseVersion: brandString<IdeFileVersion>('observed-version'), bom: true, eol: 'crlf' }],
  })
  /** Hold the next write until the returned release is called. */
  const holdNextWrite = () => {
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    writes.gate = release.promise.then(() => undefined)
    writes.entered = () => { entered.resolve(undefined) }
    return { entered: entered.promise, release: () => { release.resolve(undefined) } }
  }
  return { root, directory, store, open, projects, workspaceId, otherId, data, holdNextWrite, file: (id: WorkspaceId) => join(directory, ideStateFileName(id)) }
}

describe('durable project editor state', () => {
  it('reopens same-named files from two roots without merging their dirty text or active tab', async () => {
    const { store, open, workspaceId, data } = await fixture()
    const rootId = brandString<IdeRootId>('attached')
    const primary = data('primary draft')
    const draft: IdeWorkspaceStateData = { ...primary,
      tabs: [...primary.tabs, { ...primary.tabs[0]!, rootId }], activeRootId: rootId,
      expandedRoots: [{ rootId, path: '中文' }],
      buffers: [...primary.buffers, { ...primary.buffers[0]!, rootId, content: 'attached draft' }],
    }
    await store.replace(workspaceId, 0, draft)
    await store.close()
    const reopened = await open()
    cleanups.push(() => reopened.close())
    expect((await reopened.get(workspaceId)).data).toEqual(draft)
    expect(() => parseIdeStateRequest({ op: 'state.save', workspaceId, baseRevision: 1,
      data: { ...draft, tabs: [...draft.tabs, { ...draft.tabs[1] }] } })).toThrow()
  })

  it('restores dirty text and editor position after reopening even when the project directory is missing', async () => {
    const { root, store, open, workspaceId, otherId, data, file } = await fixture()
    const draft = data('尚未保存 😀\r\nprint(1)\r\n')
    expect(await store.get(workspaceId)).toMatchObject({ version: 1, revision: 0,
      data: { lastSessionId: null, buffers: [], layout: { agentWidth: 400, bottomHeight: 220 } } })
    await store.replace(workspaceId, 0, draft)
    expect((await store.get(otherId)).revision).toBe(0)
    expect(JSON.parse(await readFile(file(workspaceId), 'utf8'))).toEqual({ version: 1, revision: 1, data: draft })
    await store.close()
    await rm(join(root, 'a'), { recursive: true })
    const reopened = await open()
    cleanups.push(() => reopened.close())
    expect(await reopened.get(workspaceId)).toEqual({ version: 1, revision: 1, data: draft })
  })

  it('compares concurrent replacements at commit time and returns the authoritative recovery state', async () => {
    const { store, workspaceId, data, holdNextWrite } = await fixture()
    const hold = holdNextWrite()
    const first = store.replace(workspaceId, 0, data('first'))
    await hold.entered
    const second = store.replace(workspaceId, 0, data('stale'))
    hold.release()
    await expect(first).resolves.toMatchObject({ revision: 1 })
    await expect(second).rejects.toMatchObject({ code: 'revision-conflict', currentState: { revision: 1, data: { buffers: [{ content: 'first' }] } } })
    expect((await store.get(workspaceId)).data.buffers[0]?.content).toBe('first')
  })

  it('keeps committed memory and disk when atomic publication fails', async () => {
    const { store, workspaceId, data, file } = await fixture()
    await store.replace(workspaceId, 0, data('committed'))
    const before = await readFile(file(workspaceId))
    await rm(file(workspaceId))
    await mkdir(file(workspaceId))
    try {
      await expect(store.replace(workspaceId, 1, data('rejected'))).rejects.toThrow()
      expect((await store.get(workspaceId)).data.buffers[0]?.content).toBe('committed')
    } finally { await rmdir(file(workspaceId)); await writeFile(file(workspaceId), before) }
    await expect(store.replace(workspaceId, 1, data('recovered'))).resolves.toMatchObject({ revision: 2 })
  })

  it('drains admitted recovery writes on close and rejects new writes', async () => {
    const { store, workspaceId, data, holdNextWrite, file } = await fixture()
    const hold = holdNextWrite()
    const pending = store.replace(workspaceId, 0, data('drained'))
    await hold.entered
    const closing = store.close()
    await expect(store.replace(workspaceId, 0, data('new'))).rejects.toMatchObject({ code: 'closed' })
    await expect(store.get(workspaceId)).rejects.toMatchObject({ code: 'closed' })
    hold.release()
    await pending
    await closing
    expect(JSON.parse(await readFile(file(workspaceId), 'utf8'))).toMatchObject({ revision: 1 })
  })

  it('enforces configurable recovery size without advancing the revision', async () => {
    const { store, workspaceId, data } = await fixture({ maxBufferBytes: 8 })
    await expect(store.replace(workspaceId, 0, data('中文中文'))).rejects.toMatchObject({ code: 'state-too-large' })
    expect((await store.get(workspaceId)).revision).toBe(0)
    await expect(store.replace(workspaceId, 0, data('small'))).resolves.toMatchObject({ revision: 1 })
  })

  it('detaches caller objects and keeps an empty-buffer tombstone against stale recovery writes', async () => {
    const { store, workspaceId, data } = await fixture()
    const draft = data('original')
    const pending = store.replace(workspaceId, 0, draft)
    Object.assign(draft, { buffers: [] })
    const saved = await pending
    expect(saved.data.buffers[0]?.content).toBe('original')
    Object.assign(saved.data, { buffers: [] })
    expect((await store.get(workspaceId)).data.buffers[0]?.content).toBe('original')
    await store.replace(workspaceId, 1, { ...(await store.get(workspaceId)).data, buffers: [] })
    await expect(store.replace(workspaceId, 1, data('late'))).rejects.toMatchObject({ code: 'revision-conflict' })
    expect((await store.get(workspaceId)).data.buffers).toEqual([])
  })

  it('validates JSON editor fields, remembers chat identities and preserves run choices', async () => {
    const { store, workspaceId, data } = await fixture()
    const parsed = parseIdeStateRequest({ op: 'state.save', workspaceId, baseRevision: 0, data: {
      ...data('draft'), lastSessionId: 'cold-chat', execution: { profiles: [{ name: 'Python', language: 'python', program: '中文/main.py' }],
        activeProfile: 'Python', breakpoints: [{ path: '中文/main.py', lines: [2] }], watches: ['counter'] },
    } })
    if (parsed.op !== 'state.save') throw new Error('Fixture did not parse as a workspace state save.')
    const saved = await store.handle(parsed)
    expect(saved.data.lastSessionId).toBe('cold-chat')
    expect(saved.data.execution?.breakpoints).toEqual([{ path: '中文/main.py', lines: [2] }])
    for (const change of [{ apiKey: 'must-not-persist' }, { buffers: [{ ...data('draft').buffers[0], path: '../outside' }] }, { activePath: 'not-open.py' }]) {
      expect(() => parseIdeStateRequest({ op: 'state.save', workspaceId, baseRevision: 1, data: { ...data('draft'), ...change } })).toThrow()
    }
  })

  it('refuses an unregistered project and a future storage format without overwriting its bytes', async () => {
    const { store, workspaceId, otherId, projects, data, file, directory } = await fixture()
    await projects.remove(workspaceId)
    await expect(store.get(workspaceId)).rejects.toMatchObject({ code: 'workspace-not-found' })
    const future = JSON.stringify({ version: 99, revision: 4, data: {} })
    await mkdir(directory, { recursive: true })
    await writeFile(file(otherId), future)
    await expect(store.get(otherId)).rejects.toMatchObject({ code: 'io-error' })
    await expect(store.replace(otherId, 0, data('overwrite'))).rejects.toMatchObject({ code: 'io-error' })
    expect(await readFile(file(otherId), 'utf8')).toBe(future)
    await writeFile(file(otherId), '{ not json')
    await expect(store.get(otherId)).rejects.toMatchObject({ code: 'io-error' })
  })

  it('hashes identities that are not plain file names', () => {
    expect(ideStateFileName(brandString<WorkspaceId>('2f6f4c3e-8f9e-4d5c-9b3a-1c2d3e4f5a6b'))).toBe('2f6f4c3e-8f9e-4d5c-9b3a-1c2d3e4f5a6b.json')
    for (const id of ['../escape', 'CON', 'a/b', 'C:x']) expect(ideStateFileName(brandString<WorkspaceId>(id))).toMatch(/^sha256-[0-9a-f]{64}\.json$/u)
  })
})

describe('remembered project selection', () => {
  it('persists the selection across reopen without replacing recovery rows', async () => {
    const { store, open, workspaceId, data } = await fixture()
    const draft = data('retained recovery')
    await store.replace(workspaceId, 0, draft)
    expect(store.getSelection()).toEqual({ version: 1, workspaceId: null })
    await expect(store.handle({ op: 'state.selection.save', workspaceId })).resolves.toEqual({ version: 1, workspaceId })
    await store.close()
    const reopened = await open()
    cleanups.push(() => reopened.close())
    expect(await reopened.handle({ op: 'state.selection.read' })).toEqual({ version: 1, workspaceId })
    expect((await reopened.get(workspaceId)).data).toEqual(draft)
  })

  it('clears a removed registration while retaining a registered but temporarily missing directory', async () => {
    const { root, store, workspaceId, projects } = await fixture()
    await store.setSelection(workspaceId)
    await rm(join(root, 'a'), { recursive: true })
    expect(store.getSelection().workspaceId).toBe(workspaceId)
    await projects.remove(workspaceId)
    expect(store.getSelection()).toEqual({ version: 1, workspaceId: null })
    await expect(store.setSelection(workspaceId)).rejects.toMatchObject({ code: 'workspace-not-found' })
    expect(await store.setSelection(null)).toEqual({ version: 1, workspaceId: null })
    for (const value of [
      { op: 'state.selection.save', workspaceId, apiKey: 'must-not-persist' },
      { op: 'state.selection.save' }, { op: 'state.selection.read', path: '/other' },
    ]) expect(() => parseIdeStateRequest(value)).toThrow()
  })

  it('keeps the previous selection when durable publication fails', async () => {
    const { store, workspaceId, otherId, directory } = await fixture()
    await store.setSelection(workspaceId)
    const target = join(directory, 'selection.json')
    const before = await readFile(target)
    await rm(target)
    await mkdir(target)
    try {
      await expect(store.setSelection(otherId)).rejects.toThrow()
      expect(store.getSelection().workspaceId).toBe(workspaceId)
    } finally { await rmdir(target); await writeFile(target, before) }
    await expect(store.setSelection(otherId)).resolves.toEqual({ version: 1, workspaceId: otherId })
  })

  it('drains selection changes in admission order on close and refuses new changes', async () => {
    const { store, workspaceId, otherId, holdNextWrite, directory } = await fixture()
    const hold = holdNextWrite()
    const first = store.setSelection(workspaceId)
    await hold.entered
    const second = store.setSelection(otherId)
    const closing = store.close()
    await expect(store.setSelection(null)).rejects.toMatchObject({ code: 'closed' })
    hold.release()
    await Promise.all([first, second, closing])
    expect(JSON.parse(await readFile(join(directory, 'selection.json'), 'utf8'))).toEqual({ version: 1, workspaceId: otherId })
  })

  it('ignores an unreadable selection file and reports it', async () => {
    const { open, directory } = await fixture()
    await mkdir(directory, { recursive: true })
    await writeFile(join(directory, 'selection.json'), '{"version":2}')
    const log = vi.fn()
    const reopened = await open(log)
    cleanups.push(() => reopened.close())
    expect(reopened.getSelection()).toEqual({ version: 1, workspaceId: null })
    expect(log).toHaveBeenCalledWith(expect.stringContaining('IDE selection is unreadable'))
  })
})
