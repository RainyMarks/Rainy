// @vitest-environment happy-dom
/** The Git pane: changes by section, staging, discarding after confirmation, committing and the AI commit message. */
import { afterEach, expect, it, vi } from 'vitest'
import type { GitStatus } from '../../src/shared/git-protocol.ts'
import type { IdeRootId, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { IdeModel, IdeState } from '../../src/renderer/ide/ide-model.ts'
import { GitView } from '../../src/renderer/git/GitView.tsx'
import { all, button, cleanup, click, fakeHost, handle, hasText, render, toast, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const primary = 'primary' as IdeRootId
const workspaceId = 'w1' as WorkspaceId
const workspace = { workspaceId, path: '/ctf', title: 'ctf', roots: [{ rootId: primary, path: '/ctf', title: 'ctf', primary: true }] }
const calls = (method: string): unknown[] => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)

function setup() {
  const state = { workspace, buffers: {} } as unknown as IdeState
  const model = { state: { getSnapshot: () => state }, openFile: vi.fn(async () => undefined), diff: vi.fn(async () => undefined), pollNow: vi.fn(), fail: vi.fn() }
  return { model, state }
}

function serve(status: GitStatus): void {
  handle('git.status', () => status)
  for (const method of ['git.stage', 'git.unstage', 'git.discard', 'git.init'] as const) handle(method, () => undefined)
  handle('git.commit', () => ({ commit: 'abc1234' }))
  handle('git.message', () => ({ message: 'Add XOR helper' }))
}

const repository: GitStatus = {
  available: true, plain: [],
  repositories: [{ rootId: primary, title: 'ctf', branch: 'main', head: '0123456', ahead: 1, behind: 0, truncated: false, files: [
    { path: 'solve.py', unstaged: 'modified' }, { path: 'notes.txt', unstaged: 'untracked' }, { path: 'lib/new.py', staged: 'added' },
    { path: 'lib/b.py', from: 'lib/a.py', staged: 'renamed' },
  ] }],
}

afterEach(async () => {
  await cleanup()
  fakeHost.call.mockClear()
  toast.mockClear()
})

it('lists staged changes and changes, stages, unstages a rename with its source, and opens a diff', async () => {
  serve(repository)
  const { model, state } = setup()
  await render(<GitView model={model as unknown as IdeModel} state={state} />)
  await waitFor(() => { expect(hasText('main')).toBe(true) })
  expect(hasText('↑1')).toBe(true)
  expect(all('[data-git-section="staged"]').map(row => row.getAttribute('data-git-file'))).toEqual(['lib/b.py', 'lib/new.py'])
  expect(all('[data-git-section="unstaged"]').map(row => row.getAttribute('data-git-file'))).toEqual(['notes.txt', 'solve.py'])

  const solve = document.querySelector<HTMLElement>('[data-git-section="unstaged"][data-git-file="solve.py"]')!
  await click(button('暂存', solve))
  expect(calls('git.stage')).toEqual([{ workspaceId, rootId: primary, paths: ['solve.py'] }])
  const renamed = document.querySelector<HTMLElement>('[data-git-file="lib/b.py"]')!
  await click(button('取消暂存', renamed))
  expect(calls('git.unstage')).toEqual([{ workspaceId, rootId: primary, paths: ['lib/b.py', 'lib/a.py'] }])

  await click(solve.querySelector<HTMLButtonElement>('button[title^="solve.py"]')!)
  expect(model.diff).toHaveBeenCalledWith('solve.py')
  await click(document.querySelector<HTMLButtonElement>('[data-git-file="notes.txt"] button[title^="notes.txt"]')!)
  expect(model.openFile).toHaveBeenCalledWith('notes.txt')
})

it('discards after confirmation, writes the message with AI and commits what is staged', async () => {
  serve(repository)
  const { model, state } = setup()
  await render(<GitView model={model as unknown as IdeModel} state={state} />)
  await waitFor(() => { expect(hasText('main')).toBe(true) })
  await click(button('撤销更改', document.querySelector<HTMLElement>('[data-git-file="notes.txt"]')!))
  expect(hasText('撤销更改？')).toBe(true)
  await click(all<HTMLButtonElement>('[role="dialog"] button').find(item => item.textContent === '撤销更改')!)
  await waitFor(() => { expect(calls('git.discard')).toEqual([{ workspaceId, rootId: primary, paths: ['notes.txt'] }]) })
  expect(model.pollNow).toHaveBeenCalled()

  await click(button('AI 生成提交说明'))
  const message = document.querySelector<HTMLTextAreaElement>('[data-git-message]')!
  await waitFor(() => { expect(message.value).toBe('Add XOR helper') })
  await click(document.querySelector<HTMLButtonElement>('[data-git-commit]')!)
  await waitFor(() => { expect(toast).toHaveBeenCalledWith('已提交 abc1234') })
  expect(calls('git.commit')).toEqual([{ workspaceId, rootId: primary, message: 'Add XOR helper', stageAll: false }])
  expect(message.value).toBe('')
})

it('offers to stage everything when nothing is staged, and commits with Ctrl+Enter', async () => {
  serve({ ...repository, repositories: [{ ...repository.repositories[0]!, files: [{ path: 'solve.py', unstaged: 'modified' }] }] })
  const { model, state } = setup()
  await render(<GitView model={model as unknown as IdeModel} state={state} />)
  await waitFor(() => { expect(hasText('暂存全部并提交')).toBe(true) })
  const message = document.querySelector<HTMLTextAreaElement>('[data-git-message]')!
  await type(message, 'Fix the key')
  message.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', ctrlKey: true, bubbles: true }))
  await waitFor(() => { expect(calls('git.commit')).toEqual([{ workspaceId, rootId: primary, message: 'Fix the key', stageAll: true }]) })
})

it('explains a missing Git and offers to start a repository in a plain folder', async () => {
  serve({ available: false, repositories: [], plain: [] })
  const { model, state } = setup()
  const view = await render(<GitView model={model as unknown as IdeModel} state={state} />)
  await waitFor(() => { expect(document.querySelector('[data-git-missing]')).not.toBeNull() })
  await view.unmount()
  serve({ available: true, repositories: [], plain: [{ rootId: primary, title: 'ctf' }] })
  await render(<GitView model={model as unknown as IdeModel} state={state} />)
  await waitFor(() => { expect(hasText('“ctf”不是 Git 仓库。')).toBe(true) })
  await click(button('初始化仓库'))
  await waitFor(() => { expect(calls('git.init')).toEqual([{ workspaceId, rootId: primary }]) })
})
