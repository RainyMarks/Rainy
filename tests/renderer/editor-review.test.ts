// @vitest-environment happy-dom
/** Reviewing the agent's changes in the editor: keep and undo buttons on each change, and keeping or undoing all of them. */
import { afterEach, expect, it, vi } from 'vitest'
import { EditorView } from '@codemirror/view'
import { assets } from '../../src/editor/editor.ts'
import type { EditorCallbacks, EditorDocument, EditorInstance } from '../../src/renderer/ide/editor-types.ts'

const labels = {
  save: '保存', format: '格式化', sendSelection: '发送', toggleBreakpoint: '断点', gotoDefinition: '定义', findReferences: '引用', rename: '重命名',
  aiEdit: 'AI 修改', aiEditPlaceholder: '要求', aiEditGenerate: '生成', aiEditGenerating: '生成中', aiEditStop: '停止', aiEditAccept: '接受',
  aiEditReject: '拒绝', aiEditReview: '请审阅', aiEditUnchanged: '没有改动', aiEditUnavailable: '不可用',
  reviewKeep: '保留', reviewUndo: '撤销', reviewKeepTitle: '保留这处', reviewUndoTitle: '撤销这处', locale: 'zh' as const,
}

const original = 'def solve():\n    key = 1\n    return key\n\nprint(solve())\n'
const changed = 'def solve():\n    key = 7\n    return key\n\nprint(solve())\nprint("done")\n'
const document_: EditorDocument = { path: 'solve.py', uri: 'untitled:solve.py', language: 'python', text: changed, readOnly: false }
let editor: EditorInstance | undefined

afterEach(async () => {
  await editor?.dispose()
  editor = undefined
  document.body.innerHTML = ''
})

async function open(): Promise<{ view: EditorView; container: HTMLElement; review: ReturnType<typeof vi.fn>; change: ReturnType<typeof vi.fn> }> {
  const container = document.createElement('div')
  document.body.append(container)
  const review = vi.fn()
  const change = vi.fn()
  const callbacks: EditorCallbacks = {
    change, view: vi.fn(), selection: vi.fn(), problems: vi.fn(), languageState: vi.fn(),
    open: vi.fn(async () => undefined), read: vi.fn(async () => undefined), prepareEdit: vi.fn(async () => undefined),
    save: vi.fn(), format: vi.fn(), sendSelection: vi.fn(), breakpoint: vi.fn(), review,
  }
  editor = await assets.create(container, callbacks, labels)
  editor.updateDocuments([document_])
  editor.showDiff('solve.py', original, true, true)
  const dom = container.querySelector<HTMLElement>('.rainy-editor-surface:not([hidden]) .cm-editor')!
  return { view: EditorView.findFromDOM(dom)!, container, review, change }
}

const press = (element: Element): void => { element.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true })) }

it('keeps one change and undoes another with the buttons on each change', async () => {
  const { view, container, review, change } = await open()
  const keeps = container.querySelectorAll('.rainy-review-keep')
  expect(keeps).toHaveLength(2)
  expect(keeps[0]!.textContent).toBe('保留')
  expect(container.querySelector('.rainy-review-undo')?.getAttribute('title')).toBe('撤销这处')
  press(keeps[0]!)
  expect(review).toHaveBeenLastCalledWith('solve.py', { original: 'def solve():\n    key = 7\n    return key\n\nprint(solve())\n', remaining: 1 })
  expect(view.state.doc.toString()).toBe(changed)
  press(container.querySelector('.rainy-review-undo')!)
  expect(view.state.doc.toString()).toBe('def solve():\n    key = 7\n    return key\n\nprint(solve())\n')
  expect(change).toHaveBeenLastCalledWith('solve.py', view.state.doc.toString())
  expect(review).toHaveBeenLastCalledWith('solve.py', expect.objectContaining({ remaining: 0 }))
})

it('keeps or undoes every change at once', async () => {
  const first = await open()
  await editor!.action('rainy.keepAll')
  expect(first.review).toHaveBeenLastCalledWith('solve.py', { original: changed, remaining: 0 })
  expect(first.view.state.doc.toString()).toBe(changed)
  await editor!.dispose()
  editor = undefined
  document.body.innerHTML = ''

  const second = await open()
  await editor!.action('rainy.nextChange')
  await editor!.action('rainy.undoAll')
  expect(second.view.state.doc.toString()).toBe(original)
  expect(second.review).toHaveBeenLastCalledWith('solve.py', { original, remaining: 0 })
})

it('shows a plain diff without buttons outside a review', async () => {
  const { container } = await open()
  editor!.showDiff('solve.py', original, true, false)
  expect(container.querySelector('.rainy-review-keep')).toBeNull()
})
