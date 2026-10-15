// @vitest-environment happy-dom
/** Editor AI: ghost-text suggestions appear after a pause, follow typing and accept with Tab; Ctrl+K rewrites the selected lines in place until accepted or rejected. */
import { afterEach, expect, it, vi } from 'vitest'
import { EditorView, runScopeHandlers } from '@codemirror/view'
import { lineHunks } from '../../src/editor/ai.ts'
import { assets } from '../../src/editor/editor.ts'
import type { EditorCallbacks, EditorDocument, EditorInstance, EditorRewriteRequest, EditorSuggestRequest } from '../../src/renderer/ide/editor-types.ts'

const labels = {
  save: '保存', format: '格式化', sendSelection: '发送', toggleBreakpoint: '断点', gotoDefinition: '定义', findReferences: '引用', rename: '重命名',
  aiEdit: 'AI 修改', aiEditPlaceholder: '要求', aiEditGenerate: '生成', aiEditGenerating: '生成中', aiEditStop: '停止', aiEditAccept: '接受',
  aiEditReject: '拒绝', aiEditReview: '请审阅', aiEditUnchanged: '没有改动', aiEditUnavailable: '不可用', reviewKeep: '保留', reviewUndo: '撤销', reviewKeepTitle: '保留这处', reviewUndoTitle: '撤销这处',
  locale: 'zh' as const,
}

function callbacks(extra: Partial<EditorCallbacks> = {}): EditorCallbacks {
  return {
    change: vi.fn(), view: vi.fn(), selection: vi.fn(), problems: vi.fn(), languageState: vi.fn(),
    open: vi.fn(async () => undefined), read: vi.fn(async () => undefined), prepareEdit: vi.fn(async () => undefined),
    save: vi.fn(), format: vi.fn(), sendSelection: vi.fn(), breakpoint: vi.fn(), ...extra,
  }
}

const python: EditorDocument = { path: 'main.py', uri: 'untitled:main.py', language: 'python', text: 'a = 1\nb = 2\nprint(a + b)\n', readOnly: false }
let editor: EditorInstance | undefined

afterEach(async () => {
  await editor?.dispose()
  editor = undefined
  document.body.innerHTML = ''
})

async function open(extra: Partial<EditorCallbacks>, document = python): Promise<{ view: EditorView; container: HTMLElement; calls: EditorCallbacks }> {
  const container = window.document.createElement('div')
  window.document.body.append(container)
  const calls = callbacks(extra)
  editor = await assets.create(container, calls, labels)
  editor.updateDocuments([document])
  editor.show(document.path)
  const dom = container.querySelector<HTMLElement>('.rainy-editor-surface:not([hidden]) .cm-editor')!
  return { view: EditorView.findFromDOM(dom)!, container, calls }
}

const pause = (ms: number): Promise<void> => new Promise(resolve => setTimeout(resolve, ms))
const key = (view: EditorView, init: KeyboardEventInit): boolean => runScopeHandlers(view, new KeyboardEvent('keydown', init), 'editor')
const type = (view: EditorView, text: string): void => {
  const at = view.state.selection.main.head
  view.dispatch({ changes: { from: at, insert: text }, selection: { anchor: at + text.length }, userEvent: 'input.type' })
}

it('suggests after a pause, keeps the rest while the typing matches, and accepts with Tab', async () => {
  const suggest = vi.fn(async (_request: EditorSuggestRequest) => ' + 1')
  const { view, container, calls } = await open({ suggest })
  editor!.setAssist({ suggestions: true })
  view.dispatch({ selection: { anchor: 5 } })
  type(view, 'x')
  expect(suggest).not.toHaveBeenCalled()
  await pause(600)
  expect(suggest).toHaveBeenCalledTimes(1)
  expect(suggest.mock.calls[0]![0]).toMatchObject({ path: 'main.py', language: 'python', prefix: 'a = 1x', suffix: '\nb = 2\nprint(a + b)\n' })
  expect(container.querySelector('.rainy-ghost-text')?.textContent).toBe(' + 1')
  type(view, ' ')
  expect(container.querySelector('.rainy-ghost-text')?.textContent).toBe('+ 1')
  expect(key(view, { key: 'Tab' })).toBe(true)
  expect(view.state.doc.line(1).text).toBe('a = 1x + 1')
  expect(container.querySelector('.rainy-ghost-text')).toBeNull()
  expect(calls.change).toHaveBeenLastCalledWith('main.py', 'a = 1x + 1\nb = 2\nprint(a + b)\n')
  expect(view.state.selection.main.head).toBe(10)
})

it('asks nothing while suggestions are off or in the middle of code', async () => {
  const suggest = vi.fn(async () => 'zzz')
  const { view } = await open({ suggest })
  view.dispatch({ selection: { anchor: 5 } })
  type(view, 'x')
  await pause(600)
  expect(suggest).not.toHaveBeenCalled()
  editor!.setAssist({ suggestions: true })
  view.dispatch({ selection: { anchor: 1 } })
  type(view, 'y')
  await pause(600)
  expect(suggest).not.toHaveBeenCalled()
})

it('rewrites the selected lines with Ctrl+K, shows the old and new lines, and rejects or accepts', async () => {
  const rewrite = vi.fn(async (_request: EditorRewriteRequest, progress: (text: string) => void) => { progress('b = 2'); await pause(20); return 'b = 20\nc = 3' })
  const { view, container } = await open({ rewrite })
  view.dispatch({ selection: { anchor: 7, head: 9 } })
  expect(key(view, { key: 'k', ctrlKey: true })).toBe(true)
  const input = container.querySelector<HTMLInputElement>('.rainy-ai-input')!
  expect(input).not.toBeNull()
  input.value = 'make it 20 and add c'
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  await vi.waitFor(() => { expect(view.state.doc.toString()).toBe('a = 1\nb = 20\nc = 3\nprint(a + b)\n') })
  expect(rewrite.mock.calls[0]![0]).toMatchObject({ path: 'main.py', before: 'a = 1\n', selection: 'b = 2', after: '\nprint(a + b)\n', instruction: 'make it 20 and add c' })
  expect(container.querySelector('.rainy-ai-removed')?.textContent).toBe('b = 2')
  expect(container.querySelectorAll('.rainy-ai-added')).toHaveLength(2)
  expect(container.querySelector('.rainy-ai-status')?.textContent).toBe(labels.aiEditReview)

  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
  expect(view.state.doc.toString()).toBe(python.text)
  expect(container.querySelector('.rainy-ai-prompt')).toBeNull()

  view.dispatch({ selection: { anchor: 7 } })
  key(view, { key: 'k', ctrlKey: true })
  const again = container.querySelector<HTMLInputElement>('.rainy-ai-input')!
  again.value = 'insert'
  again.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  await vi.waitFor(() => { expect(container.querySelector('.rainy-ai-added')).not.toBeNull() })
  // An empty selection inserts at the cursor.
  expect(rewrite.mock.calls[1]![0]).toMatchObject({ selection: '', before: 'a = 1\nb', after: ' = 2\nprint(a + b)\n' })
  expect(key(view, { key: 'Enter', ctrlKey: true })).toBe(true)
  expect(view.state.doc.toString()).toBe('a = 1\nbb = 20\nc = 3 = 2\nprint(a + b)\n')
  expect(container.querySelector('.rainy-ai-prompt')).toBeNull()
  expect(container.querySelector('.rainy-ai-added')).toBeNull()
})

it('compares the old and new lines so only changed lines are marked', () => {
  expect(lineHunks('a\nb', 'a\nb')).toEqual([])
  expect(lineHunks('return x', 'if not x:\n    raise E\nreturn x')).toEqual([{ at: 0, end: 2, removed: [] }])
  expect(lineHunks('a\nb\nc', 'a\nB\nc')).toEqual([{ at: 1, end: 2, removed: ['b'] }])
  expect(lineHunks('a\nb\nc\nd', 'a\nc')).toEqual([{ at: 1, end: 1, removed: ['b'] }, { at: 2, end: 2, removed: ['d'] }])
  expect(lineHunks('x\na\ny\nb', 'a\nz\nb\nw')).toEqual([
    { at: 0, end: 0, removed: ['x'] }, { at: 1, end: 2, removed: ['y'] }, { at: 3, end: 4, removed: [] }])
})

it('marks only the changed lines of a rewrite and shows removed last lines below the region', async () => {
  const rewrite = vi.fn(async () => 'if a:\n    raise E\nb = 2')
  const { view, container } = await open({ rewrite })
  view.dispatch({ selection: { anchor: 6, head: 11 } })
  key(view, { key: 'k', ctrlKey: true })
  const input = container.querySelector<HTMLInputElement>('.rainy-ai-input')!
  input.value = 'check a'
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  await vi.waitFor(() => { expect(container.querySelectorAll('.rainy-ai-added')).toHaveLength(2) })
  expect(container.querySelector('.rainy-ai-removed')).toBeNull()
  expect([...container.querySelectorAll('.rainy-ai-added')].map(line => line.textContent)).toEqual(['if a:', '    raise E'])
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }))
  // A rewrite that drops the region's last line shows it below the region.
  rewrite.mockResolvedValueOnce('a = 1')
  // happy-dom reports a focused editor's selection change while CodeMirror is still writing it.
  view.contentDOM.blur()
  view.dispatch({ selection: { anchor: 0, head: 11 } })
  key(view, { key: 'k', ctrlKey: true })
  const again = container.querySelector<HTMLInputElement>('.rainy-ai-input')!
  again.value = 'drop b'
  again.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  await vi.waitFor(() => { expect(view.state.doc.toString()).toBe('a = 1\nprint(a + b)\n') })
  expect(container.querySelector('.rainy-ai-added')).toBeNull()
  const removed = container.querySelector('.rainy-ai-removed')!
  expect(removed.textContent).toBe('b = 2')
  // It sits after the first line, below the region.
  const lines = [...container.querySelector('.cm-content')!.children]
  expect(lines.indexOf(removed.closest('.cm-content > *')!)).toBeGreaterThan(lines.findIndex(line => line.textContent === 'a = 1'))
})

it('shows a failure in the prompt and leaves the code alone; Markdown keeps Ctrl+K for its preview chord', async () => {
  const rewrite = vi.fn(async () => { throw new Error('没有可用的模型') })
  const { view, container } = await open({ rewrite })
  key(view, { key: 'k', ctrlKey: true })
  const input = container.querySelector<HTMLInputElement>('.rainy-ai-input')!
  input.value = 'do it'
  input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
  await vi.waitFor(() => { expect(container.querySelector('.rainy-ai-status')?.textContent).toBe('没有可用的模型') })
  expect(container.querySelector('.rainy-ai-status')?.classList.contains('rainy-ai-error')).toBe(true)
  expect(view.state.doc.toString()).toBe(python.text)
  await editor!.dispose()
  document.body.innerHTML = ''
  const markdown = await open({ rewrite }, { path: 'notes.md', uri: 'untitled:notes.md', language: 'markdown', text: '# hi\n', readOnly: false })
  expect(key(markdown.view, { key: 'k', ctrlKey: true })).toBe(false)
  await editor!.action('rainy.aiEdit')
  expect(markdown.container.querySelector('.rainy-ai-input')).not.toBeNull()
})
