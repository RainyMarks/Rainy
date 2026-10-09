// @vitest-environment jsdom
/** Quick-open results belong to one query, project, and dialog lifetime. */
import { afterEach, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { bindSnapshotSelector } from '@deepseek-ai/dsh-client-test-runtime'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import { QuickOpenDialog } from '../src/client/QuickOpenDialog.tsx'
import { IdeModel } from '../src/client/ide-model.ts'
import type { WorkspaceId } from '../src/ide-files-protocol.ts'
import { zh } from '../src/client/locales.ts'

const owners: IdeModel[] = []
afterEach(() => { cleanup(); for (const owner of owners.splice(0)) owner.dispose(); vi.restoreAllMocks() })
const t = ((key: keyof typeof zh) => zh[key]) as TranslateNS<'rainy'>
type Search = Awaited<ReturnType<IdeModel['searchFiles']>>

function fixture() {
  const model = new IdeModel({ request: vi.fn() }, { debounceMs: 60_000, pollMs: 60_000, restoreSession: vi.fn(async () => {}) })
  owners.push(model)
  model.state.set({ ...model.state.getSnapshot(), phase: 'ready', quickOpen: true,
    workspace: { workspaceId: 'a' as WorkspaceId, path: '/project-a', title: 'A' } })
  const search = vi.spyOn(model, 'searchFiles').mockResolvedValue({ paths: ['old.py'], truncated: false })
  const openFile = vi.spyOn(model, 'openFile').mockResolvedValue(undefined)
  const useIde = bindSnapshotSelector(model.state)
  function Page() { return <QuickOpenDialog model={model} state={useIde(value => value)} t={t} /> }
  return { model, search, openFile, mount: () => render(<Page />) }
}

it('waits for the current query before accepting Enter or a file choice', async () => {
  const h = fixture()
  h.mount()
  await screen.findByRole('option', { name: 'old.py' })
  const next = Promise.withResolvers<Search>()
  h.search.mockReturnValueOnce(next.promise)
  fireEvent.change(screen.getByRole('combobox'), { target: { value: 'new' } })
  fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' })
  expect(h.openFile).not.toHaveBeenCalled()
  expect(screen.queryAllByRole('option')).toHaveLength(0)
  await expect(`${screen.getByRole('status').getAttribute('aria-label')}\n`)
    .toMatchFileSnapshot('./expected/quick-open-pending.txt')
  await act(async () => { next.resolve({ paths: ['new.py'], truncated: false }); await next.promise })
  fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' })
  await waitFor(() => { expect(h.openFile).toHaveBeenCalledExactlyOnceWith('new.py') })
})

it('keeps later results when an aborted earlier query resolves last', async () => {
  const h = fixture()
  const earlier = Promise.withResolvers<Search>()
  const later = Promise.withResolvers<Search>()
  h.search.mockReturnValueOnce(earlier.promise).mockReturnValueOnce(later.promise)
  h.mount()
  fireEvent.change(screen.getByRole('combobox'), { target: { value: 'new' } })
  expect(h.search.mock.calls[0]?.[1]?.aborted).toBe(true)
  await act(async () => { later.resolve({ paths: ['new.py'], truncated: false }); await later.promise })
  await act(async () => { earlier.resolve({ paths: ['old.py'], truncated: true }); await earlier.promise })
  expect(screen.queryByRole('option', { name: 'old.py' })).toBeNull()
  expect(screen.queryByText(zh.ideSearchTruncated)).toBeNull()
  fireEvent.click(screen.getByRole('option', { name: 'new.py' }))
  await waitFor(() => { expect(h.openFile).toHaveBeenCalledExactlyOnceWith('new.py') })
})

it('keeps a failed query empty and does not submit its text or an earlier result', async () => {
  const h = fixture()
  h.mount()
  await screen.findByRole('option', { name: 'old.py' })
  h.search.mockRejectedValueOnce(new Error('Search unavailable'))
  fireEvent.change(screen.getByRole('combobox'), { target: { value: 'new' } })
  await screen.findByRole('alert')
  fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' })
  expect(h.openFile).not.toHaveBeenCalled()
  expect(screen.queryAllByRole('option')).toHaveLength(0)
})

it.each(['project', 'reopen'] as const)('starts empty after %s while ignoring the previous dialog request', async (change) => {
  const h = fixture()
  const previous = Promise.withResolvers<Search>()
  const current = Promise.withResolvers<Search>()
  h.search.mockReturnValueOnce(previous.promise).mockReturnValueOnce(current.promise)
  h.mount()
  if (change === 'project') act(() => {
    h.model.state.set({ ...h.model.state.getSnapshot(), workspace: { workspaceId: 'b' as WorkspaceId, path: '/project-b', title: 'B' } })
  })
  else {
    fireEvent.click(screen.getByRole('button', { name: zh.ideClose }))
    expect(screen.queryByRole('dialog')).toBeNull()
    act(() => { h.model.quickOpen(true) })
  }
  expect(h.search.mock.calls[0]?.[1]?.aborted).toBe(true)
  await act(async () => { previous.resolve({ paths: ['previous.py'], truncated: false }); await previous.promise })
  fireEvent.keyDown(screen.getByRole('combobox'), { key: 'Enter' })
  expect(h.openFile).not.toHaveBeenCalled()
  expect(screen.queryAllByRole('option')).toHaveLength(0)
  await act(async () => { current.resolve({ paths: ['current.py'], truncated: false }); await current.promise })
  fireEvent.click(screen.getByRole('option', { name: 'current.py' }))
  await waitFor(() => { expect(h.openFile).toHaveBeenCalledExactlyOnceWith('current.py') })
})

it('does not close a reopened dialog when an earlier file open completes', async () => {
  const h = fixture()
  const opening = Promise.withResolvers<undefined>()
  h.openFile.mockReturnValueOnce(opening.promise)
  h.mount()
  fireEvent.click(await screen.findByRole('option', { name: 'old.py' }))
  fireEvent.click(screen.getByRole('button', { name: zh.ideClose }))
  act(() => { h.model.quickOpen(true) })
  await screen.findByRole('option', { name: 'old.py' })
  await act(async () => { opening.resolve(undefined); await opening.promise })
  expect(screen.getByRole('dialog')).toBeTruthy()
})
