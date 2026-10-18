// @vitest-environment happy-dom
/** The changed-files list above the composer: review, keep and undo per file and for all. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { ChatChange, RevertResult } from '../../src/shared/rpc.ts'
import { on } from '../../src/renderer/app/bus.ts'
import { getPrefs } from '../../src/renderer/prefs.ts'
import { ChangesBar } from '../../src/renderer/review/ChangesBar.tsx'
import { all, button, cleanup, click, emit, fakeHost, flush, handle, hasText, render, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const solve: ChatChange = {
  path: '/ctf/baby/solve.py', display: 'solve.py', toolCallIds: ['w1', 'e2'], created: false, deleted: false, added: 4, removed: 1, comparable: true, undoable: true,
}
const helper: ChatChange = {
  path: '/ctf/baby/lib/helper.py', display: 'lib/helper.py', toolCallIds: ['w3'], created: true, deleted: false, added: 7, removed: 0, comparable: true, undoable: true,
}
let changes: ChatChange[]

beforeEach(() => {
  changes = [solve, helper]
  handle('chat.changes', () => changes)
  handle('chat.keep', ({ toolCallIds }) => { changes = changes.filter(change => !change.toolCallIds.some(id => toolCallIds.includes(id))) })
  handle('chat.revert', ({ toolCallIds }): RevertResult => {
    changes = changes.filter(change => !change.toolCallIds.some(id => toolCallIds.includes(id)))
    return { reverted: toolCallIds.map(toolCallId => ({ toolCallId, path: 'x' })), failed: [] }
  })
})
afterEach(async () => {
  await cleanup()
  fakeHost.call.mockClear()
})

function rowButton(display: string, name: string): HTMLButtonElement {
  const row = document.querySelector(`[data-change="${display}"]`)
  if (row === null) throw new Error(`no row for ${display}`)
  return button(name, row)
}

it('lists the changed files with their counts and keeps, undoes or reviews each', async () => {
  await render(<ChangesBar sessionId="s1" busy={false} />)
  await waitFor(() => { expect(hasText('AI 改了 2 个文件')).toBe(true) })
  expect(hasText('+11')).toBe(true)
  await click(document.querySelector<HTMLButtonElement>('[data-changes] button[aria-expanded]')!)
  expect(all('[data-change]').map(row => row.getAttribute('data-change'))).toEqual(['solve.py', 'lib/helper.py'])
  expect(hasText('新建')).toBe(true)

  const reviews: unknown[] = []
  const stop = on('review.open', (payload) => { reviews.push(payload) })
  await click(rowButton('solve.py', '审查'))
  stop()
  expect(reviews).toEqual([{ sessionId: 's1', path: '/ctf/baby/solve.py', toolCallIds: ['w1', 'e2'] }])

  await click(rowButton('solve.py', '保留'))
  await waitFor(() => { expect(all('[data-change]')).toHaveLength(1) })
  expect(fakeHost.call).toHaveBeenCalledWith('chat.keep', { sessionId: 's1', toolCallIds: ['w1', 'e2'] })

  await click(rowButton('lib/helper.py', '撤销'))
  await waitFor(() => { expect(document.querySelector('[data-changes]')).toBeNull() })
  expect(fakeHost.call).toHaveBeenCalledWith('chat.revert', { sessionId: 's1', toolCallIds: ['w3'] })
})

it('says the changes are kept with the next message while Settings keeps them automatically', async () => {
  const hint = '发出下一条消息时会自动保留这些修改（可在“设置 → 安全”中关闭）'
  await render(<ChangesBar sessionId="s1" busy={false} />)
  await waitFor(() => { expect(hasText('AI 改了 2 个文件')).toBe(true) })
  await click(document.querySelector<HTMLButtonElement>('[data-changes] button[aria-expanded]')!)
  expect(hasText(hint)).toBe(true)
  await emit('prefs.changed', { ...getPrefs(), autoKeepChanges: false })
  expect(hasText(hint)).toBe(false)
  expect(all('[data-change]')).toHaveLength(2)
  await emit('prefs.changed', { ...getPrefs(), autoKeepChanges: true, rainEffects: true, webSearch: true })
})

it('undoes every file after confirmation, refreshes on Host notice, and waits for the chat to finish before undoing', async () => {
  const view = await render(<ChangesBar sessionId="s1" busy />)
  await waitFor(() => { expect(hasText('AI 改了 2 个文件')).toBe(true) })
  expect(button('全部撤销').disabled).toBe(true)
  await view.rerender(<ChangesBar sessionId="s1" busy={false} />)
  await click(button('全部撤销'))
  expect(hasText('撤销 AI 对这些文件的全部改动？')).toBe(true)
  await click(all<HTMLButtonElement>('[role="dialog"] button').find(item => item.textContent === '全部撤销')!)
  await waitFor(() => { expect(document.querySelector('[data-changes]')).toBeNull() })
  expect(fakeHost.call).toHaveBeenCalledWith('chat.revert', { sessionId: 's1', toolCallIds: ['w1', 'e2', 'w3'] })

  changes = [solve]
  await emit('session.changes', { sessionId: 's1' })
  await new Promise(resolve => setTimeout(resolve, 200))
  await flush()
  expect(hasText('AI 改了 1 个文件')).toBe(true)
})
