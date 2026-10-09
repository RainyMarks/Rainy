/** First-message gestures retain browser contents and create one controller handoff. */
import { describe, expect, it, vi } from 'vitest'
import type { WorkspaceId } from '../src/ide-files-protocol.ts'
import { DeferredDraft, publishPreparedDraft } from '../src/client/deferred-draft.ts'

describe('DeferredDraft', () => {
  it('captures one project and coalesces rapid submits while preserving file order', async () => {
    let selected = 'a' as WorkspaceId
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    const handoff = vi.fn(async () => { entered.resolve(undefined); await release.promise })
    const draft = new DeferredDraft({ workspace: () => selected, handoff })
    const files = [new File(['one'], 'one.txt'), new File(['two'], 'two.txt')]
    draft.change('first message\n')
    draft.addFiles(files)
    const first = draft.submit()
    const second = draft.submit()
    expect(first).toBe(second)
    await entered.promise
    selected = 'b' as WorkspaceId
    draft.change('late edit')
    draft.removeFile(0)
    expect(handoff).toHaveBeenCalledOnce()
    expect(handoff).toHaveBeenCalledWith('a', { text: 'first message\n', files }, expect.any(Function), expect.any(AbortSignal))
    release.resolve(undefined)
    await first
    expect(draft.state.getSnapshot()).toEqual({ text: '', files: [], busy: false, error: '' })
  })

  it('keeps the original text and browser files when creation or handoff fails', async () => {
    const handoff = vi.fn().mockRejectedValueOnce(new Error('creation refused')).mockResolvedValue(undefined)
    const draft = new DeferredDraft({ workspace: () => 'a' as WorkspaceId, handoff })
    const file = new File(['keep'], 'keep.txt')
    draft.change('keep text')
    draft.addFiles([file])
    await draft.submit()
    expect(draft.state.getSnapshot()).toEqual({ text: 'keep text', files: [file], busy: false, error: 'creation refused' })
    await draft.submit()
    expect(handoff).toHaveBeenCalledTimes(2)
    expect(draft.state.getSnapshot().files).toEqual([])
  })

  it('does not create a Session before a project and nonempty text or files are selected', async () => {
    let selected: WorkspaceId | null = null
    const handoff = vi.fn(async () => {})
    const draft = new DeferredDraft({ workspace: () => selected, handoff })
    draft.change('waiting')
    await draft.submit()
    expect(handoff).not.toHaveBeenCalled()
    selected = 'a' as WorkspaceId
    draft.change('')
    await draft.submit()
    expect(handoff).not.toHaveBeenCalled()
    draft.addFiles([new File(['attachment only'], 'note.txt')])
    await draft.submit()
    expect(handoff).toHaveBeenCalledOnce()
  })

  it('does not restore a duplicate after the normal composer adopted the text and file', async () => {
    const draft = new DeferredDraft({ workspace: () => 'a' as WorkspaceId,
      handoff: async (_id, _contents, adopt) => { adopt(); throw new Error('upload failed') } })
    draft.change('owned by session')
    draft.addFiles([new File(['content'], 'owned.txt')])
    await draft.submit()
    expect(draft.state.getSnapshot()).toEqual({ text: '', files: [], busy: false, error: '' })
  })

  it('aborts late continuations and joins admitted creation before disposing', async () => {
    const entered = Promise.withResolvers<undefined>()
    const created = Promise.withResolvers<undefined>()
    const open = vi.fn()
    const draft = new DeferredDraft({ workspace: () => 'a' as WorkspaceId,
      handoff: async (_id, _contents, _adopt, signal) => {
        entered.resolve(undefined)
        await created.promise
        signal.throwIfAborted()
        open()
      } })
    draft.change('keep')
    void draft.submit()
    await entered.promise
    let disposed = false
    const disposing = draft.dispose().then(() => { disposed = true })
    await Promise.resolve()
    expect(disposed).toBe(false)
    created.resolve(undefined)
    await disposing
    expect(open).not.toHaveBeenCalled()
    expect(draft.state.getSnapshot()).toMatchObject({ text: 'keep', busy: false, error: '' })
    await draft.submit()
    expect(open).not.toHaveBeenCalled()
  })

  it('keeps a slow upload hidden and publishes the captured draft without an editable interval', async () => {
    const upload = Promise.withResolvers<undefined>()
    const actions: string[] = []
    const draft = new DeferredDraft({ workspace: () => 'a' as WorkspaceId,
      handoff: async (_id, contents, adopt) => publishPreparedDraft(upload.promise, {
        assertCurrent: () => {}, reveal: () => { adopt(); actions.push('reveal') },
        submit: () => { actions.push(contents.text) }, fail: () => { actions.push('failure') },
      }) })
    draft.change('captured')
    const sending = draft.submit()
    await Promise.resolve()
    draft.change('changed during upload')
    expect(actions).toEqual([])
    expect(draft.state.getSnapshot()).toMatchObject({ text: 'captured', busy: true })
    upload.resolve(undefined)
    await sending
    expect(actions).toEqual(['reveal', 'captured'])
  })

  it('restores browser recovery after navigation supersedes a hidden upload, without revealing or sending it', async () => {
    const upload = Promise.withResolvers<undefined>()
    const navigation = new AbortController()
    const reveal = vi.fn()
    const submit = vi.fn()
    const file = new File(['keep'], 'keep.txt')
    const draft = new DeferredDraft({ workspace: () => 'a' as WorkspaceId,
      handoff: async (_id, _contents, adopt) => publishPreparedDraft(upload.promise, {
        assertCurrent: () => { navigation.signal.throwIfAborted() },
        reveal: () => { adopt(); reveal() }, submit, fail: vi.fn(),
      }) })
    draft.change('keep')
    draft.addFiles([file])
    const sending = draft.submit()
    await Promise.resolve()
    navigation.abort()
    upload.resolve(undefined)
    await sending
    expect(reveal).not.toHaveBeenCalled()
    expect(submit).not.toHaveBeenCalled()
    expect(draft.state.getSnapshot()).toEqual({ text: 'keep', files: [file], busy: false, error: '' })
  })

  it('reveals an upload failure for normal retry without submitting or keeping a duplicate browser draft', async () => {
    const actions: string[] = []
    const draft = new DeferredDraft({ workspace: () => 'a' as WorkspaceId,
      handoff: async (_id, _contents, adopt) => publishPreparedDraft(Promise.reject(new Error('upload refused')), {
        assertCurrent: () => {}, reveal: () => { adopt(); actions.push('reveal') },
        submit: () => { actions.push('submit') }, fail: () => { actions.push('retry') },
      }) })
    draft.change('keep in normal composer')
    await draft.submit()
    expect(actions).toEqual(['reveal', 'retry'])
    expect(draft.state.getSnapshot()).toEqual({ text: '', files: [], busy: false, error: '' })
  })
})
