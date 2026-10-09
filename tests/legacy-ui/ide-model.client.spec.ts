/** Recovery races and source conflict handling without timers, a browser, or a live filesystem. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { IdeModel, sourceUri } from '../src/client/ide-model.ts'
import { IdeRequestError, type IdeFilesApi } from '../src/client/ide-api.ts'
import type {
  IdeFileDocument,
  IdeFileVersion,
  IdeWorkspace,
  IdeWorkspaceState,
  WorkspaceId,
  IdeRootId,
} from '../src/ide-files-protocol.ts'
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import { fileKey } from '../src/client/ide-paths.ts'

const a: IdeWorkspace = { workspaceId: 'a' as WorkspaceId, path: '/tmp/project a', title: 'A' }
const b: IdeWorkspace = { workspaceId: 'b' as WorkspaceId, path: '/tmp/project-b', title: 'B' }
const version = 'v1' as IdeFileVersion
const state = (revision = 0): IdeWorkspaceState => ({
  version: 1,
  revision,
  data: {
    lastSessionId: null,
    tabs: [],
    activePath: null,
    expandedPaths: [],
    buffers: [],
    layout: {
      sidebarWidth: 240,
      agentWidth: 400,
      bottomHeight: 240,
      sidebarVisible: true,
      agentVisible: true,
      bottomVisible: true,
      bottomTab: 'terminal',
    },
  },
})
const document = (content = 'initial\n', fileVersion = version): IdeFileDocument => ({
  workspaceId: a.workspaceId,
  path: 'main.py',
  content,
  version: fileVersion,
  bytes: content.length,
  bom: false,
  eol: 'lf',
  readOnlyReason: null,
})
const models: IdeModel[] = []
afterEach(() => {
  for (const model of models.splice(0)) model.dispose()
})

async function fixture(isSessionSelected?: (sessionId: SessionId) => boolean) {
  const request = vi.fn<IdeFilesApi['request']>()
  const restoreSession = vi.fn(async () => {})
  const model = new IdeModel(
    { request: request as IdeFilesApi['request'] },
    { debounceMs: 60_000, pollMs: 60_000, restoreSession, isSessionSelected },
  )
  models.push(model)
  request.mockResolvedValueOnce([a, b]).mockResolvedValueOnce({ version: 1, workspaceId: null })
  await model.initialize()
  request.mockResolvedValueOnce(state()).mockResolvedValueOnce({ path: '', entries: [] })
    .mockResolvedValueOnce({ version: 1, workspaceId: a.workspaceId })
  await model.selectWorkspace(a)
  request.mockResolvedValueOnce(document())
  await model.openFile('main.py')
  return { request, model, restoreSession }
}

describe('workspace recovery', () => {
  it('leaves a newer history selection intact when an older project restore finishes', async () => {
    const { model, request, restoreSession } = await fixture(() => false)
    request.mockResolvedValueOnce(state(1)).mockResolvedValueOnce(state()).mockResolvedValueOnce({ path: '', entries: [] })
    await model.selectWorkspace(b, 'superseded-chat' as SessionId)
    expect(model.state.getSnapshot()).toMatchObject({ workspace: a, phase: 'ready' })
    expect(restoreSession).toHaveBeenCalledTimes(1)
    expect(request.mock.calls.filter(([body]) => body.op === 'state.selection.save')).toHaveLength(1)
  })

  it('keeps a history-selected chat instead of restoring that project\'s older remembered chat', async () => {
    const { model, request, restoreSession } = await fixture()
    const selected = 'selected-history' as SessionId
    request.mockResolvedValueOnce(state(1))
      .mockResolvedValueOnce({ ...state(), data: { ...state().data, lastSessionId: 'older-chat' as SessionId } })
      .mockResolvedValueOnce({ path: '', entries: [] })
      .mockResolvedValueOnce({ version: 1, workspaceId: b.workspaceId })
    await model.selectWorkspace(b, selected)
    expect(model.state.getSnapshot().data.lastSessionId).toBe(selected)
    expect(restoreSession).toHaveBeenLastCalledWith(b, selected)
  })

  it('shows the AI panel for a chat opened from history even when that project saved it hidden', async () => {
    const { model, request } = await fixture()
    const hidden = (revision = 0) => ({ ...state(revision), data: { ...state(revision).data,
      layout: { ...state(revision).data.layout, agentVisible: false } } })
    request.mockResolvedValueOnce(state(1)).mockResolvedValueOnce(hidden())
      .mockResolvedValueOnce({ path: '', entries: [] }).mockResolvedValueOnce({ version: 1, workspaceId: b.workspaceId })
    await model.selectWorkspace(b, 'from-history' as SessionId)
    expect(model.state.getSnapshot().data.layout.agentVisible).toBe(true)
    request.mockResolvedValueOnce(state(2)).mockResolvedValueOnce(hidden())
      .mockResolvedValueOnce({ path: '', entries: [] }).mockResolvedValueOnce({ version: 1, workspaceId: a.workspaceId })
    await model.selectWorkspace(a)
    expect(model.state.getSnapshot().data.layout.agentVisible).toBe(false)
  })

  it('creates a file at an attached root and blocks unmounting its unsaved buffer', async () => {
    const { model, request } = await fixture()
    const rootId = 'attached' as IdeRootId
    const mounted: IdeWorkspace = { ...a, roots: [
      { rootId: 'primary' as IdeRootId, path: a.path, title: a.title, primary: true },
      { rootId, path: '/shared', title: 'Shared', primary: false },
    ] }
    request.mockResolvedValueOnce(state(1)).mockResolvedValueOnce(mounted)
      .mockResolvedValueOnce({ path: '', entries: [] }).mockResolvedValueOnce({ path: '', entries: [] })
    await model.attachRoot('/shared')
    const created = { ...document(''), path: 'new.py' }
    request.mockResolvedValueOnce(created).mockResolvedValueOnce({ path: '', entries: [] }).mockResolvedValueOnce(created)
    const key = fileKey('new.py', rootId)
    await model.create(key, false)
    expect(request).toHaveBeenCalledWith({ op: 'files.list', workspaceId: a.workspaceId, rootId, path: '' })
    expect(model.state.getSnapshot().data.activePath).toBe(key)
    model.change(key, 'unsaved attachment')
    await expect(model.removeRoot(rootId)).rejects.toMatchObject({ code: 'unsaved-root' })
    expect(request.mock.calls.some(([body]) => body.op === 'workspaces.removeRoot')).toBe(false)
  })

  it('prunes deleted expanded folders before publishing the new project and matching conversation', async () => {
    const { model, request, restoreSession } = await fixture()
    request.mockResolvedValueOnce(state(1))
      .mockResolvedValueOnce({ ...state(), data: { ...state().data, expandedPaths: ['deleted', 'kept'] } })
      .mockResolvedValueOnce({ path: '', entries: [] })
      .mockRejectedValueOnce(new IdeRequestError('not-found', 'The selected path no longer exists.'))
      .mockResolvedValueOnce({ path: 'kept', entries: [] })
      .mockResolvedValueOnce({ version: 1, workspaceId: b.workspaceId })
    await model.selectWorkspace(b)
    expect(model.state.getSnapshot()).toMatchObject({ workspace: b, phase: 'ready', error: '', data: { expandedPaths: ['kept'] } })
    expect(restoreSession).toHaveBeenLastCalledWith(b, null)
    expect(request).toHaveBeenLastCalledWith({ op: 'state.selection.save', workspaceId: b.workspaceId })
  })

  it('restores the previous conversation and buffers if committing the next project selection fails', async () => {
    const { model, request, restoreSession } = await fixture()
    request.mockResolvedValueOnce(state(1)).mockResolvedValueOnce(state())
      .mockResolvedValueOnce({ path: '', entries: [] })
      .mockRejectedValueOnce(new Error('selection storage failed'))
    await model.selectWorkspace(b)
    expect(model.state.getSnapshot()).toMatchObject({ workspace: a, phase: 'ready', error: 'selection storage failed',
      buffers: { 'main.py': { text: 'initial\n' } } })
    expect(restoreSession).toHaveBeenLastCalledWith(a, null)
  })

  it('publishes readiness only after the selected workspace is durably remembered', async () => {
    const { model, request } = await fixture()
    let finishSave: ((value: { version: 1; workspaceId: WorkspaceId }) => void) | undefined
    request.mockResolvedValueOnce(state(1))
      .mockResolvedValueOnce(state())
      .mockResolvedValueOnce({ path: '', entries: [] })
      .mockImplementationOnce(() => new Promise((resolve) => { finishSave = resolve }))
    const selection = model.selectWorkspace(b)
    expect(model.state.getSnapshot().phase).toBe('loading')
    await vi.waitFor(() => { expect(finishSave).toBeDefined() })
    expect(model.state.getSnapshot()).toMatchObject({ workspace: a, phase: 'loading' })
    if (finishSave === undefined) throw new Error('The selection save did not start')
    finishSave({ version: 1, workspaceId: b.workspaceId })
    await selection
    expect(model.state.getSnapshot().phase).toBe('ready')
    request.mockResolvedValueOnce({ ...document('next\n'), workspaceId: b.workspaceId, path: 'next.py' })
    await model.openFile('next.py')
    expect(model.state.getSnapshot().data.activePath).toBe('next.py')
  })

  it('restores the Host-selected workspace and its unsaved buffer without browser-local preferences', async () => {
    const request = vi.fn<IdeFilesApi['request']>()
    const restoreSession = vi.fn(async () => {})
    const model = new IdeModel({ request: request as IdeFilesApi['request'] }, {
      debounceMs: 60_000, pollMs: 60_000, restoreSession,
    })
    models.push(model)
    const saved = state(4)
    const sessionId = 'saved-session' as SessionId
    request.mockResolvedValueOnce([a, b])
      .mockResolvedValueOnce({ version: 1, workspaceId: a.workspaceId })
      .mockResolvedValueOnce({ ...saved, data: { ...saved.data, tabs: [{ path: 'main.py', kind: 'file' }],
        activePath: 'main.py', lastSessionId: sessionId,
        buffers: [{ path: 'main.py', content: 'restored draft\n', baseVersion: version, bom: false, eol: 'lf' }] } })
      .mockResolvedValueOnce(document())
      .mockResolvedValueOnce({ path: '', entries: [] })
      .mockResolvedValueOnce({ version: 1, workspaceId: a.workspaceId })
    await model.initialize()
    expect(request).toHaveBeenCalledWith({ op: 'state.selection.read' })
    expect(model.state.getSnapshot()).toMatchObject({ workspace: a,
      buffers: { 'main.py': { text: 'restored draft\n', dirty: true } } })
    expect(restoreSession).toHaveBeenCalledWith(a, sessionId)
    expect(request).toHaveBeenLastCalledWith({ op: 'state.selection.save', workspaceId: a.workspaceId })
    expect(request.mock.calls.some(([body]) => body.op === 'files.save')).toBe(false)
  })

  it('stores edits independently, then restores another workspace and its selected chat', async () => {
    const { model, request, restoreSession } = await fixture()
    model.change('main.py', 'private draft\n')
    const next = state(3)
    const chat = 'chat-b' as SessionId
    request
      .mockResolvedValueOnce(state(1))
      .mockResolvedValueOnce({ ...next, data: { ...next.data, lastSessionId: chat } })
      .mockResolvedValueOnce({ path: '', entries: [] })
      .mockResolvedValueOnce({ version: 1, workspaceId: b.workspaceId })
    await model.selectWorkspace(b)
    const recovery = request.mock.calls.find(([request]) => request.op === 'state.save')?.[0]
    if (recovery?.op !== 'state.save') throw new Error('Recovery was not saved')
    expect(recovery.workspaceId).toBe(a.workspaceId)
    expect(recovery.baseRevision).toBe(0)
    expect(recovery.data.buffers).toEqual([
      { path: 'main.py', content: 'private draft\n', baseVersion: version, bom: false, eol: 'lf' },
    ])
    expect(model.state.getSnapshot().workspace).toEqual(b)
    expect(model.state.getSnapshot().buffers).toEqual({})
    expect(restoreSession).toHaveBeenLastCalledWith(b, chat)
    expect(request.mock.calls.some(([request]) => request.op === 'files.save')).toBe(false)
  })

  it('persists a second snapshot when edits arrive during the first recovery save', async () => {
    const { model, request } = await fixture()
    model.change('main.py', 'first')
    let complete: ((value: IdeWorkspaceState) => void) | undefined
    request
      .mockImplementationOnce(
        () =>
          new Promise((resolve) => {
            complete = resolve
          }),
      )
      .mockResolvedValueOnce(state(2))
    const flushing = model.flush()
    model.change('main.py', 'second')
    if (complete === undefined) throw new Error('The first save did not start')
    complete(state(1))
    expect(await flushing).toBe(true)
    const recovery = request.mock.calls.at(-1)?.[0]
    if (recovery?.op !== 'state.save') throw new Error('Recovery was not saved')
    expect(recovery.baseRevision).toBe(1)
    expect(recovery.data.buffers[0]?.content).toBe('second')
  })

  it('retains the current workspace and dirty text when recovery CAS rejects a concurrent writer', async () => {
    const { model, request } = await fixture()
    model.change('main.py', 'unsaved')
    request.mockRejectedValueOnce(new IdeRequestError('revision-conflict', 'conflicting window', state(2)))
    await model.selectWorkspace(b)
    expect(model.state.getSnapshot()).toMatchObject({
      workspace: a,
      recoveryConflict: true,
      buffers: { 'main.py': { text: 'unsaved', dirty: true } },
    })
    expect(await model.flush()).toBe(false)
    expect(request.mock.calls.filter(([request]) => request.op === 'state.read')).toHaveLength(1)
    expect(request.mock.calls.filter(([request]) => request.op === 'state.selection.save')).toEqual([
      [{ op: 'state.selection.save', workspaceId: a.workspaceId }],
    ])
  })
})

describe('complete source and conflicts', () => {
  it('compares AI code in a readonly preview without replacing an unsaved source buffer', async () => {
    const { model, request } = await fixture()
    model.change('main.py', 'human draft')
    model.openSnippet('assistant proposal', 'python', 'Snippet', true)
    const snapshot = model.state.getSnapshot()
    const path = snapshot.data.activePath
    if (path === null) throw new Error('Preview tab did not open')
    expect(snapshot.buffers['main.py']?.text).toBe('human draft')
    expect(snapshot.buffers[path]).toMatchObject({
      source: 'snippet',
      text: 'assistant proposal',
      dirty: false,
      document: { workspaceId: null, version: null },
      comparison: { original: 'human draft', kind: 'snippet', target: 'main.py' },
    })
    model.change(path, 'attempted preview edit')
    expect(model.state.getSnapshot().buffers[path]?.text).toBe('assistant proposal')
    request.mockResolvedValueOnce(state(1))
    expect(await model.flush()).toBe(true)
    const saved = request.mock.calls.at(-1)?.[0]
    if (saved?.op !== 'state.save') throw new Error('Recovery was not saved')
    expect(saved.data.tabs.map(tab => tab.path)).toEqual(['main.py'])
    expect(saved.data.buffers[0]?.content).toBe('human draft')
    expect(request.mock.calls.some(([request]) => request.op === 'files.save')).toBe(false)
  })

  it('requires a refreshed diff review if the target changed before applying an AI preview', async () => {
    const { model, request } = await fixture()
    model.openSnippet('assistant proposal', 'py', 'Snippet', true)
    const path = model.state.getSnapshot().data.activePath
    if (path === null) throw new Error('Preview tab did not open')
    model.change('main.py', 'new human edit')
    expect(await model.applySnippet(path)).toBe('changed')
    expect(model.state.getSnapshot().buffers['main.py']?.text).toBe('new human edit')
    expect(model.state.getSnapshot().buffers[path]?.comparison?.original).toBe('new human edit')
    expect(await model.applySnippet(path)).toBe('applied')
    expect(model.state.getSnapshot().buffers['main.py']).toMatchObject({ text: 'assistant proposal', dirty: true })
    expect(request.mock.calls.some(([request]) => request.op === 'files.save')).toBe(false)
  })

  it('keeps all lines and encodes filename characters in absolute source URIs', async () => {
    const { model, request } = await fixture()
    const content = Array.from({ length: 600 }, (_, index) => `line ${index}\n`).join('')
    request.mockResolvedValueOnce({ ...document(content), path: 'full #.py' })
    await model.openFile('full #.py')
    expect(model.state.getSnapshot().buffers['full #.py']?.text).toBe(content)
    expect(sourceUri(a, 'full #.py')).toBe('file:///tmp/project%20a/full%20%23.py')
  })

  it('retains an unopened language edit as a background tab, recovery buffer, and versioned save', async () => {
    const { model, request } = await fixture()
    const unopened = { ...document('answer()\n', 'helper-v1' as IdeFileVersion), path: 'helper.py' }
    request.mockResolvedValueOnce(unopened)
    expect(await model.openUri(sourceUri(a, 'helper.py'), 'edit')).toMatchObject({ path: 'helper.py', text: 'answer()\n' })
    expect(model.state.getSnapshot().data.activePath).toBe('main.py')
    expect(model.state.getSnapshot().data.tabs).toEqual([{ path: 'main.py', kind: 'file' }, { path: 'helper.py', kind: 'file' }])
    model.change('main.py', 'renamed_answer = 1\n')
    model.change('helper.py', 'renamed_answer()\n')
    request.mockResolvedValueOnce(state(1))
    expect(await model.flush()).toBe(true)
    const recovery = request.mock.calls.at(-1)?.[0]
    if (recovery?.op !== 'state.save') throw new Error('Recovery was not saved')
    expect(recovery.data.buffers).toEqual([
      { path: 'main.py', content: 'renamed_answer = 1\n', baseVersion: version, bom: false, eol: 'lf' },
      { path: 'helper.py', content: 'renamed_answer()\n', baseVersion: 'helper-v1', bom: false, eol: 'lf' },
    ])
    request.mockResolvedValueOnce(document('renamed_answer = 1\n', 'v2' as IdeFileVersion))
      .mockResolvedValueOnce({ ...unopened, content: 'renamed_answer()\n', version: 'helper-v2' as IdeFileVersion })
    expect(await model.saveAll()).toBe(true)
    expect(request).toHaveBeenLastCalledWith({ op: 'files.save', workspaceId: a.workspaceId, path: 'helper.py',
      content: 'renamed_answer()\n', expectedVersion: 'helper-v1' })
    expect(Object.values(model.state.getSnapshot().buffers).every(buffer => !buffer.dirty)).toBe(true)
  })

  it('shows disk and local text after a save conflict and accepts only the displayed disk version', async () => {
    const { model, request } = await fixture()
    model.change('main.py', 'local edit')
    request
      .mockRejectedValueOnce(new IdeRequestError('version-conflict', 'disk changed'))
      .mockResolvedValueOnce(document('external edit', 'v2' as IdeFileVersion))
    expect(await model.save()).toBe(false)
    expect(model.state.getSnapshot().buffers['main.py']).toMatchObject({
      text: 'local edit',
      dirty: true,
      external: true,
      comparison: { original: 'external edit', kind: 'conflict', version: 'v2' },
    })
    expect(model.state.getSnapshot().data.tabs).toEqual([{ path: 'main.py', kind: 'diff' }])
    const calls = request.mock.calls.length
    model.acceptConflict('main.py')
    expect(request.mock.calls).toHaveLength(calls)
    request.mockResolvedValueOnce(document('local edit', 'v3' as IdeFileVersion))
    expect(await model.save()).toBe(true)
    expect(request).toHaveBeenLastCalledWith(
      expect.objectContaining({ op: 'files.save', expectedVersion: 'v2', content: 'local edit' }),
    )
  })

  it('reports a save conflict and keeps the buffer when the disk copy was deleted', async () => {
    const { model, request } = await fixture()
    model.change('main.py', 'local edit')
    request
      .mockRejectedValueOnce(new IdeRequestError('version-conflict', 'disk changed'))
      .mockRejectedValueOnce(new IdeRequestError('not-found', 'gone'))
    expect(await model.save()).toBe(false)
    expect(model.state.getSnapshot().error).toBe('disk changed')
    expect(model.state.getSnapshot().buffers['main.py']).toMatchObject({ text: 'local edit', dirty: true })
  })

  it('retains edits made while a disk save is pending', async () => {
    const { model, request } = await fixture()
    model.change('main.py', 'first')
    let complete: ((value: IdeFileDocument) => void) | undefined
    request.mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          complete = resolve
        }),
    )
    const saving = model.save()
    model.change('main.py', 'newer')
    if (complete === undefined) throw new Error('The file save did not start')
    complete(document('first', 'v2' as IdeFileVersion))
    expect(await saving).toBe(true)
    expect(model.state.getSnapshot().buffers['main.py']).toMatchObject({
      text: 'newer',
      dirty: true,
      document: { content: 'first', version: 'v2' },
    })
  })

  it('keeps binary metadata out of editable source and ignores external debug sources', async () => {
    const { model, request } = await fixture()
    request.mockResolvedValueOnce({ ...document(''), path: 'image.bin', content: null, readOnlyReason: 'binary' })
    await model.openFile('image.bin')
    model.change('image.bin', 'replacement')
    expect(model.state.getSnapshot().buffers['image.bin']).toMatchObject({ text: '', dirty: false })
    const count = request.mock.calls.length
    await model.reveal('/usr/lib/python3.12/os.py', 12, 1)
    expect(request.mock.calls).toHaveLength(count)
  })
})
