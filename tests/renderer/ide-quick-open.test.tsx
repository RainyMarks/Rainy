// @vitest-environment happy-dom
/** Quick-open results belong to one query, project, and dialog lifetime. */
import { act } from 'react'
import { afterEach, expect, it, vi } from 'vitest'
import { QuickOpenDialog } from '../../src/renderer/ide/QuickOpenDialog.tsx'
import { IdeModel } from '../../src/renderer/ide/ide-model.ts'
import { useStore } from '../../src/renderer/ui/store.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { allByRole, allByText, byRole, change, cleanup, click, findByRole, keyDown, queryByRole, render, waitFor } from './ide-dom.tsx'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

const owners: IdeModel[] = []
afterEach(() => { cleanup(); for (const owner of owners.splice(0)) owner.dispose(); vi.restoreAllMocks() })
type Search = Awaited<ReturnType<IdeModel['searchFiles']>>

function fixture() {
  const model = new IdeModel({ request: vi.fn() }, { debounceMs: 60_000, pollMs: 60_000, restoreSession: vi.fn(async () => {}) })
  owners.push(model)
  model.state.set({ ...model.state.getSnapshot(), phase: 'ready', quickOpen: true,
    workspace: { workspaceId: 'a' as WorkspaceId, path: '/project-a', title: 'A' } })
  const search = vi.spyOn(model, 'searchFiles').mockResolvedValue({ paths: ['old.py'], truncated: false })
  const openFile = vi.spyOn(model, 'openFile').mockResolvedValue(undefined)
  function Page() { return <QuickOpenDialog model={model} state={useStore(model.state)} /> }
  return { model, search, openFile, mount: () => render(<Page />) }
}

it('waits for the current query before accepting Enter or a file choice', async () => {
  const h = fixture()
  h.mount()
  await findByRole('option', 'old.py')
  const next = Promise.withResolvers<Search>()
  h.search.mockReturnValueOnce(next.promise)
  change(byRole('combobox'), 'new')
  keyDown(byRole('combobox'), { key: 'Enter' })
  expect(h.openFile).not.toHaveBeenCalled()
  expect(allByRole('option')).toHaveLength(0)
  expect(byRole('status').getAttribute('aria-label')).toBe('正在加载工作区…')
  await act(async () => { next.resolve({ paths: ['new.py'], truncated: false }); await next.promise })
  keyDown(byRole('combobox'), { key: 'Enter' })
  await waitFor(() => { expect(h.openFile).toHaveBeenCalledExactlyOnceWith('new.py') })
  await waitFor(() => { expect(h.model.state.getSnapshot().quickOpen).toBe(false) })
})

it('keeps later results when an aborted earlier query resolves last', async () => {
  const h = fixture()
  const earlier = Promise.withResolvers<Search>()
  const later = Promise.withResolvers<Search>()
  h.search.mockReturnValueOnce(earlier.promise).mockReturnValueOnce(later.promise)
  h.mount()
  change(byRole('combobox'), 'new')
  expect(h.search.mock.calls[0]?.[1]?.aborted).toBe(true)
  await act(async () => { later.resolve({ paths: ['new.py'], truncated: false }); await later.promise })
  await act(async () => { earlier.resolve({ paths: ['old.py'], truncated: true }); await earlier.promise })
  expect(queryByRole('option', 'old.py')).toBeNull()
  expect(allByText('结果较多，请输入更具体的文件名')).toHaveLength(0)
  click(byRole('option', 'new.py'))
  await waitFor(() => { expect(h.openFile).toHaveBeenCalledExactlyOnceWith('new.py') })
})

it('keeps a failed query empty and does not submit its text or an earlier result', async () => {
  const h = fixture()
  h.mount()
  await findByRole('option', 'old.py')
  h.search.mockRejectedValueOnce(new Error('Search unavailable'))
  change(byRole('combobox'), 'new')
  expect((await findByRole('alert')).textContent).toBe('Search unavailable')
  keyDown(byRole('combobox'), { key: 'Enter' })
  expect(h.openFile).not.toHaveBeenCalled()
  expect(allByRole('option')).toHaveLength(0)
})

it.each(['project', 'reopen'] as const)('starts empty after %s while ignoring the previous dialog request', async (kind) => {
  const h = fixture()
  const previous = Promise.withResolvers<Search>()
  const current = Promise.withResolvers<Search>()
  h.search.mockReturnValueOnce(previous.promise).mockReturnValueOnce(current.promise)
  h.mount()
  if (kind === 'project') act(() => {
    h.model.state.set({ ...h.model.state.getSnapshot(), workspace: { workspaceId: 'b' as WorkspaceId, path: '/project-b', title: 'B' } })
  })
  else {
    click(byRole('button', '关闭'))
    expect(queryByRole('dialog')).toBeNull()
    act(() => { h.model.quickOpen(true) })
  }
  expect(h.search.mock.calls[0]?.[1]?.aborted).toBe(true)
  await act(async () => { previous.resolve({ paths: ['previous.py'], truncated: false }); await previous.promise })
  keyDown(byRole('combobox'), { key: 'Enter' })
  expect(h.openFile).not.toHaveBeenCalled()
  expect(allByRole('option')).toHaveLength(0)
  await act(async () => { current.resolve({ paths: ['current.py'], truncated: false }); await current.promise })
  click(byRole('option', 'current.py'))
  await waitFor(() => { expect(h.openFile).toHaveBeenCalledExactlyOnceWith('current.py') })
})

it('does not close a reopened dialog when an earlier file open completes', async () => {
  const h = fixture()
  const opening = Promise.withResolvers<undefined>()
  h.openFile.mockReturnValueOnce(opening.promise)
  h.mount()
  click(await findByRole('option', 'old.py'))
  click(byRole('button', '关闭'))
  act(() => { h.model.quickOpen(true) })
  await findByRole('option', 'old.py')
  await act(async () => { opening.resolve(undefined); await opening.promise })
  expect(byRole('dialog')).toBeTruthy()
})
