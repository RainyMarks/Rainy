// @vitest-environment happy-dom
/** The search pane: searching as you type, opening a match, dismissing files, and replacing on disk and in unsaved buffers. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { IdeRootId, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { TextReplaceRequest, TextSearchRequest, TextSearchResult } from '../../src/shared/text-search.ts'
import type { IdeModel, IdeState } from '../../src/renderer/ide/ide-model.ts'
import { SearchView } from '../../src/renderer/search/SearchView.tsx'
import { all, button, cleanup, click, fakeHost, handle, hasText, render, toast, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const primary = 'primary' as IdeRootId
const workspace = { workspaceId: 'w1' as WorkspaceId, path: '/ctf', title: 'ctf', roots: [{ rootId: primary, path: '/ctf', title: 'ctf', primary: true }] }
const found: TextSearchResult = {
  matches: 3, truncated: false,
  files: [
    { rootId: primary, path: 'solve.py', matches: [
      { line: 1, column: 0, length: 3, preview: 'key = b"r41n"', previewColumn: 0 },
      { line: 2, column: 6, length: 3, preview: 'print(key)', previewColumn: 6 },
    ] },
    { rootId: primary, path: 'docs/notes.md', matches: [{ line: 1, column: 4, length: 3, preview: 'the key', previewColumn: 4 }] },
  ],
}
let searches: TextSearchRequest[]
let replaces: TextReplaceRequest[]

function setup(buffers: Record<string, { text: string; dirty: boolean }> = {}) {
  const state = { workspace, buffers } as unknown as IdeState
  const model = {
    state: { getSnapshot: () => state },
    reveal: vi.fn(async () => undefined), change: vi.fn(), pollNow: vi.fn(), fail: vi.fn(),
  }
  return { model, state }
}

beforeEach(() => {
  searches = []
  replaces = []
  handle('search.text', (request) => { searches.push(request); return found })
  handle('search.replace', (request) => {
    replaces.push(request)
    return { files: request.files.map(file => ({ ...file, replaced: 2 })), failed: [] }
  })
})
afterEach(async () => {
  await cleanup()
  fakeHost.call.mockClear()
  toast.mockClear()
})

it('searches as you type, groups matches by file and opens a match with it selected', async () => {
  const { model, state } = setup()
  await render(<SearchView model={model as unknown as IdeModel} state={state} />)
  await type(document.querySelector<HTMLInputElement>('[data-search-input]')!, 'key')
  await waitFor(() => { expect(hasText('2 个文件中有 3 个结果')).toBe(true) })
  expect(searches.at(-1)).toEqual({ workspaceId: 'w1', pattern: 'key', regex: false, caseSensitive: false, wholeWord: false, include: '', exclude: '' })
  expect(all('[data-search-file]').map(row => row.getAttribute('data-search-file'))).toEqual(['solve.py', 'docs/notes.md'])
  await click(all<HTMLButtonElement>('[data-search-match]')[1]!)
  expect(model.reveal).toHaveBeenCalledWith('solve.py', 2, 7, 3)

  await click(button('区分大小写'))
  await click(button('使用正则表达式'))
  await waitFor(() => { expect(searches.at(-1)).toMatchObject({ caseSensitive: true, regex: true }) })

  const notes = document.querySelector<HTMLElement>('[data-search-file="docs/notes.md"]')!
  await click(button('从结果中移除', notes))
  expect(hasText('1 个文件中有 2 个结果')).toBe(true)
})

it('replaces every match: unsaved buffers in the editor, other files on disk', async () => {
  const { model, state } = setup({ 'docs/notes.md': { text: 'the key and key\n', dirty: true } })
  await render(<SearchView model={model as unknown as IdeModel} state={state} focus={{ at: 1, text: 'key', replace: true }} />)
  await waitFor(() => { expect(hasText('2 个文件中有 3 个结果')).toBe(true) })
  await type(document.querySelector<HTMLInputElement>('[data-replace-input]')!, 'secret')
  // The preview shows each match crossed out with its replacement after it.
  expect(all('ins').map(element => element.textContent)).toContain('secret')
  await click(button('全部替换'))
  expect(hasText('全部替换？')).toBe(true)
  await click(all<HTMLButtonElement>('[role="dialog"] button').find(item => item.textContent === '全部替换')!)
  await waitFor(() => { expect(toast).toHaveBeenCalledWith('已在 2 个文件中替换 4 处') })
  expect(model.change).toHaveBeenCalledWith('docs/notes.md', 'the secret and secret\n')
  expect(replaces).toEqual([{ workspaceId: 'w1', pattern: 'key', regex: false, caseSensitive: false, wholeWord: false, include: '', exclude: '',
    replacement: 'secret', files: [{ rootId: 'primary', path: 'solve.py' }] }])
  expect(model.pollNow).toHaveBeenCalled()
})

it('passes file filters and shows the Host error for a bad pattern', async () => {
  handle('search.text', (request) => { searches.push(request); throw new Error('正则表达式有误：unclosed group') })
  const { model, state } = setup()
  await render(<SearchView model={model as unknown as IdeModel} state={state} />)
  await click(button('显示或隐藏文件筛选'))
  await type(document.querySelector<HTMLInputElement>('input[placeholder="例如 src, *.py"]')!, 'src')
  await type(document.querySelector<HTMLInputElement>('[data-search-input]')!, '(')
  await waitFor(() => { expect(hasText('正则表达式有误：unclosed group')).toBe(true) })
  expect(searches.at(-1)).toMatchObject({ pattern: '(', include: 'src' })
})
