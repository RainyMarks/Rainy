/** Atomic recovery rows, revision conflicts and independent workspace state on the real JSON backend. */
import { mkdir, mkdtemp, readFile, rename, rm, rmdir, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Context } from '@deepseek-ai/cordis'
import { brandString } from '@deepseek-ai/dsh-brand'
import Storage from '@deepseek-ai/dsh-storage'
import { DomainFacility } from '@deepseek-ai/dsh-storage-domain'
import { JsonStorageBackend } from '@deepseek-ai/dsh-storage-json'
import type { Workspace, WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type { IdeFileVersion, IdeRootId, IdeWorkspaceStateData } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'
import { RainyIdeStateStore, ideStateSpec, parseIdeStateRequest, resolveIdeStateConfig } from '../src/ide-state.ts'
import { restorePendingHostProject } from '../src/host-project.ts'
import { createProjectRegistry, ExecutionTargetId } from '../src/project-registry.ts'
import RainyProjectRoots from '../src/project-roots.ts'

const cleanups: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
  vi.restoreAllMocks()
})

function project(id: WorkspaceId): Workspace {
  return { id, path: '/missing/workspace', title: 'fixture', createdAt: '2026-01-01T00:00:00.000Z', updatedAt: '2026-01-01T00:00:00.000Z', sessionIds: [],
    setTitle: async () => {}, attachSession: async () => {}, insertSessionBefore: async () => {}, detachSession: async () => {}, status: async () => 'missing-dir' }
}

async function fixture(config: Parameters<typeof resolveIdeStateConfig>[0] = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rainy-ide-state-'))
  cleanups.push(() => rm(root, { recursive: true, force: true }))
  const ctx = new Context()
  cleanups.push(() => ctx.fiber.dispose())
  await ctx.plugin(Storage)
  const backend = new JsonStorageBackend(root)
  cleanups.push(() => backend.close())
  const unregister = ctx.storage.backend.register('json', backend)
  cleanups.push(async () => { unregister() })
  const facility = new DomainFacility(ctx, { backend: 'json', routes: {} })
  const workspaceId = brandString<WorkspaceId>('workspace-a')
  const otherId = brandString<WorkspaceId>('workspace-b')
  const records = new Map([[workspaceId, project(workspaceId)], [otherId, project(otherId)]])
  const registry = { get: (id: WorkspaceId) => records.get(id) }
  const domain = await facility.open(ideStateSpec)
  const resolved = resolveIdeStateConfig(config)
  const store = new RainyIdeStateStore({ domain, registry, config: resolved })
  cleanups.push(() => store.close())
  const data = (text: string): IdeWorkspaceStateData => ({
    ...store.get(workspaceId).data,
    tabs: [{ path: '中文/main.py', kind: 'file', cursor: { line: 2, column: 3 }, scroll: { top: 30, left: 0 } }],
    activePath: '中文/main.py', expandedPaths: ['中文'],
    buffers: [{ path: '中文/main.py', content: text, baseVersion: brandString<IdeFileVersion>('observed-version'), bom: true, eol: 'crlf' }],
  })
  return { root, ctx, store, domain, facility, registry, records, workspaceId, otherId, data, config: resolved }
}

describe('Rainy IDE durable workspace state', () => {
  it('reopens same-named files from two roots without merging their dirty text or active tab', async () => {
    const { store, facility, workspaceId, registry, config, data } = await fixture()
    const rootId = brandString<IdeRootId>('attached')
    const primary = data('primary draft')
    const draft: IdeWorkspaceStateData = { ...primary,
      tabs: [...primary.tabs, { ...primary.tabs[0], rootId }], activeRootId: rootId,
      expandedRoots: [{ rootId, path: '中文' }],
      buffers: [...primary.buffers, { ...primary.buffers[0], rootId, content: 'attached draft' }],
    }
    await store.replace(workspaceId, 0, draft)
    await store.close()
    const reopened = new RainyIdeStateStore({ domain: await facility.open(ideStateSpec), registry, config })
    cleanups.push(() => reopened.close())
    expect(reopened.get(workspaceId).data).toEqual(draft)
    expect(() => parseIdeStateRequest({ op: 'state.save', workspaceId, baseRevision: 1,
      data: { ...draft, tabs: [...draft.tabs, { ...draft.tabs[1] }] } })).toThrow()
  })

  it('restores dirty text and editor position after reopening without touching missing project files', async () => {
    const { root, store, facility, workspaceId, otherId, registry, config, data } = await fixture()
    const draft = data('尚未保存 😀\r\nprint(1)\r\n')
    expect(store.get(workspaceId)).toMatchObject({ version: 1, revision: 0,
      data: { lastSessionId: null, buffers: [], layout: { agentWidth: 400 } } })
    await store.replace(workspaceId, 0, draft)
    expect(store.get(otherId).revision).toBe(0)
    const persisted: unknown = JSON.parse(await readFile(join(root, 'rainy_ide.json'), 'utf8'))
    expect(persisted).toMatchObject({ tables: { workspaces: { [workspaceId]: { data: { buffers: draft.buffers } } } } })
    await store.close()
    const reopened = new RainyIdeStateStore({ domain: await facility.open(ideStateSpec), registry, config })
    cleanups.push(() => reopened.close())
    expect(reopened.get(workspaceId)).toEqual({ version: 1, revision: 1, data: draft })
  })

  it('compares concurrent replacements at commit time and returns the authoritative recovery state', async () => {
    const { store, domain, workspaceId, data } = await fixture()
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    const table = domain.table('workspaces')
    const put = table.put.bind(table)
    vi.spyOn(table, 'put').mockImplementationOnce(async (...args) => { entered.resolve(undefined); await release.promise; return put(...args) })
    const first = store.replace(workspaceId, 0, data('first'))
    await entered.promise
    const second = store.replace(workspaceId, 0, data('stale'))
    release.resolve(undefined)
    await expect(first).resolves.toMatchObject({ revision: 1 })
    await expect(second).rejects.toMatchObject({ code: 'revision-conflict', currentState: { revision: 1, data: { buffers: [{ content: 'first' }] } } })
    expect(store.get(workspaceId).data.buffers[0]?.content).toBe('first')
  })

  it('keeps committed memory and disk when atomic publication fails', async () => {
    const { root, store, workspaceId, data } = await fixture()
    await store.replace(workspaceId, 0, data('committed'))
    const target = join(root, 'rainy_ide.json')
    const backup = join(root, 'committed.json')
    const before = await readFile(target)
    await rename(target, backup)
    await mkdir(target)
    try {
      await expect(store.replace(workspaceId, 1, data('rejected'))).rejects.toThrow()
      expect(store.get(workspaceId).data.buffers[0]?.content).toBe('committed')
      expect(await readFile(backup)).toEqual(before)
    } finally { await rmdir(target); await rename(backup, target) }
    await expect(store.replace(workspaceId, 1, data('recovered'))).resolves.toMatchObject({ revision: 2 })
  })

  it('drains admitted recovery writes on close and rejects new writes', async () => {
    const { root, store, domain, workspaceId, data } = await fixture()
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    const table = domain.table('workspaces')
    const put = table.put.bind(table)
    vi.spyOn(table, 'put').mockImplementationOnce(async (...args) => { entered.resolve(undefined); await release.promise; return put(...args) })
    const pending = store.replace(workspaceId, 0, data('drained'))
    await entered.promise
    const rejected = data('new')
    const closing = store.close()
    await expect(store.replace(workspaceId, 0, rejected)).rejects.toMatchObject({ code: 'closed' })
    release.resolve(undefined)
    await pending
    await closing
    const persisted: unknown = JSON.parse(await readFile(join(root, 'rainy_ide.json'), 'utf8'))
    expect(persisted).toMatchObject({ tables: { workspaces: { [workspaceId]: { revision: 1 } } } })
  })

  it('enforces configurable recovery size without advancing the revision', async () => {
    const { store, workspaceId, data } = await fixture({ maxBufferBytes: 8 })
    await expect(store.replace(workspaceId, 0, data('中文中文'))).rejects.toMatchObject({ code: 'state-too-large' })
    expect(store.get(workspaceId).revision).toBe(0)
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
    expect(store.get(workspaceId).data.buffers[0]?.content).toBe('original')
    await store.replace(workspaceId, 1, { ...store.get(workspaceId).data, buffers: [] })
    await expect(store.replace(workspaceId, 1, data('late'))).rejects.toMatchObject({ code: 'revision-conflict' })
    expect(store.get(workspaceId).data.buffers).toEqual([])
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

  it('refuses an unregistered workspace and a future storage format without overwriting its bytes', async () => {
    const { root, store, workspaceId, facility, records } = await fixture()
    records.delete(workspaceId)
    expect(() => store.get(workspaceId)).toThrow()
    await store.close()
    const target = join(root, 'rainy_ide.json')
    const future = JSON.stringify({ unit: { name: 'rainy_ide', version: 99 }, global: null, tables: { workspaces: {} } })
    await writeFile(target, future)
    await expect(facility.open(ideStateSpec)).rejects.toThrow()
    expect(await readFile(target, 'utf8')).toBe(future)
  })
})

describe('Rainy IDE selected workspace', () => {
  it('restores a migrated project selection before reopening while preserving existing drafts, roots and session links', async () => {
    const { root, ctx, store, facility, registry, records, workspaceId, otherId, data, config } = await fixture()
    const previousPath = join(root, 'previous')
    const targetPath = join(root, 'target')
    const attachedPath = join(root, 'attached')
    await Promise.all([mkdir(previousPath), mkdir(targetPath), mkdir(attachedPath)])
    records.set(workspaceId, { ...project(workspaceId), path: previousPath })
    const target = { ...project(otherId), path: targetPath }
    records.set(otherId, target)
    ctx.provide('workspaceRegistry', { ...registry, list: () => [...records.values()], create: async () => target })
    ctx.provide('storageDomain', facility)
    ctx.provide('rainyIdeState', store)
    await ctx.plugin(RainyProjectRoots, { maxRoots: 16 })
    await vi.waitFor(() => { expect(ctx.get('rainyProjectRoots')?.get(otherId)).toHaveLength(1) })
    const attached = (await ctx.rainyProjectRoots.attach(otherId, attachedPath))[1]
    const oldDraft = { ...data('previous Host draft'), lastSessionId: brandString<import('@deepseek-ai/dsh-session').SessionId>('old-session') }
    const targetDraft = { ...data('target Host draft'), activeRootId: attached.rootId,
      tabs: data('').tabs.map(tab => ({ ...tab, rootId: attached.rootId })),
      buffers: data('target Host draft').buffers.map(buffer => ({ ...buffer, rootId: attached.rootId })) }
    await store.replace(workspaceId, 0, oldDraft)
    await store.replace(otherId, 0, targetDraft)
    await store.setSelection(workspaceId)
    const carrierRoot = join(root, 'carrier')
    const sourceTarget = ExecutionTargetId('wsl:source')
    const nativeTarget = ExecutionTargetId('windows-local')
    const source = createProjectRegistry({ root: carrierRoot, targetId: sourceTarget })
    const publicId = await source.getOrRegister({ workspaceId: brandString<WorkspaceId>('source-workspace'), path: '/project', title: 'Source' })
    const projects = createProjectRegistry({ root: carrierRoot, targetId: nativeTarget })
    await projects.getOrRegister({ workspaceId: otherId, path: targetPath, title: 'Target', projectId: publicId })
    await projects.selectTarget(publicId, sourceTarget)
    ctx.provide('rainyRuntime', { projects })
    const rootsBefore = ctx.rainyProjectRoots.get(otherId)

    await restorePendingHostProject(ctx, { projectId: publicId, targetId: nativeTarget, path: targetPath,
      roots: [{ rootId: attached.rootId, path: attached.path, title: attached.title }] })

    expect(store.getSelection().workspaceId).toBe(otherId)
    expect(store.get(workspaceId)).toEqual({ version: 1, revision: 1, data: oldDraft })
    expect(store.get(otherId)).toEqual({ version: 1, revision: 1, data: targetDraft })
    expect(ctx.rainyProjectRoots.get(otherId)).toEqual(rootsBefore)
    expect((await projects.list())[0]).toMatchObject({ projectId: publicId, activeTargetId: nativeTarget })
    expect((await projects.list())[0]?.bindings).toHaveLength(2)
    await store.close()
    const reopened = new RainyIdeStateStore({ domain: await facility.open(ideStateSpec), registry, config })
    cleanups.push(() => reopened.close())
    expect(reopened.getSelection().workspaceId).toBe(otherId)
    expect(reopened.get(workspaceId).data).toEqual(oldDraft)
    expect(reopened.get(otherId).data).toEqual(targetDraft)
  })

  it('opens an older domain without a selection and persists the selection across reopen without replacing recovery rows', async () => {
    const { root, store, facility, workspaceId, registry, config, data } = await fixture()
    const draft = data('retained recovery')
    await store.replace(workspaceId, 0, draft)
    await store.close()
    const old = JSON.stringify({ unit: { name: 'rainy_ide', version: 1 }, global: null,
      tables: { workspaces: { [workspaceId]: { version: 1, revision: 1, data: draft } } } })
    await writeFile(join(root, 'rainy_ide.json'), old)
    const upgraded = new RainyIdeStateStore({ domain: await facility.open(ideStateSpec), registry, config })
    cleanups.push(() => upgraded.close())
    expect(upgraded.getSelection()).toEqual({ version: 1, workspaceId: null })
    expect(await readFile(join(root, 'rainy_ide.json'), 'utf8')).toBe(old)
    await expect(upgraded.handle({ op: 'state.selection.save', workspaceId })).resolves.toEqual({ version: 1, workspaceId })
    expect(upgraded.get(workspaceId)).toEqual({ version: 1, revision: 1, data: draft })
    await upgraded.close()
    const reopened = new RainyIdeStateStore({ domain: await facility.open(ideStateSpec), registry, config })
    cleanups.push(() => reopened.close())
    expect(await reopened.handle({ op: 'state.selection.read' })).toEqual({ version: 1, workspaceId })
    expect(reopened.get(workspaceId).data).toEqual(draft)
  })

  it('clears a deleted registration while retaining a registered but temporarily missing directory', async () => {
    const { store, workspaceId, records } = await fixture()
    await store.setSelection(workspaceId)
    expect(store.getSelection().workspaceId).toBe(workspaceId)
    records.delete(workspaceId)
    expect(store.getSelection()).toEqual({ version: 1, workspaceId: null })
    await expect(store.setSelection(workspaceId)).rejects.toMatchObject({ code: 'workspace-not-found' })
    expect(await store.setSelection(null)).toEqual({ version: 1, workspaceId: null })
    for (const value of [
      { op: 'state.selection.save', workspaceId, apiKey: 'must-not-persist' },
      { op: 'state.selection.save' }, { op: 'state.selection.read', path: '/other' },
    ]) expect(() => parseIdeStateRequest(value)).toThrow()
  })

  it('keeps the previous selection when durable publication fails', async () => {
    const { root, store, workspaceId, otherId } = await fixture()
    await store.setSelection(workspaceId)
    const target = join(root, 'rainy_ide.json')
    const backup = join(root, 'selection.json')
    const before = await readFile(target)
    await rename(target, backup)
    await mkdir(target)
    try {
      await expect(store.setSelection(otherId)).rejects.toThrow()
      expect(store.getSelection().workspaceId).toBe(workspaceId)
      expect(await readFile(backup)).toEqual(before)
    } finally { await rmdir(target); await rename(backup, target) }
    await expect(store.setSelection(otherId)).resolves.toEqual({ version: 1, workspaceId: otherId })
  })

  it('drains selection changes in admission order on close and refuses new changes', async () => {
    const { root, store, domain, workspaceId, otherId } = await fixture()
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    const save = domain.global.set.bind(domain.global)
    vi.spyOn(domain.global, 'set').mockImplementationOnce(async (value) => {
      entered.resolve(undefined)
      await release.promise
      await save(value)
    })
    const first = store.setSelection(workspaceId)
    await entered.promise
    const second = store.setSelection(otherId)
    const closing = store.close()
    await expect(store.setSelection(null)).rejects.toMatchObject({ code: 'closed' })
    release.resolve(undefined)
    await Promise.all([first, second, closing])
    const persisted: unknown = JSON.parse(await readFile(join(root, 'rainy_ide.json'), 'utf8'))
    expect(persisted).toMatchObject({ global: { version: 1, workspaceId: otherId }, tables: { workspaces: {} } })
  })
})
