// @vitest-environment happy-dom
/** The CodeMirror editor reports user edits but not text the IDE pushes, shows breakpoints and comparisons, and keeps language-server input safe. */
import { afterEach, expect, it, vi } from 'vitest'
import { EditorView } from '@codemirror/view'
import { assets } from '../../src/editor/editor.ts'
import { languageKey, lspLanguageId, serverLanguage } from '../../src/editor/languages.ts'
import { sanitizeDocumentation } from '../../src/editor/lsp.ts'
import { directoryUri, uriInside, uriKey } from '../../src/editor/uri.ts'
import type { EditorCallbacks, EditorDocument, EditorInstance } from '../../src/renderer/ide/editor-types.ts'

const labels = {
  save: '保存', format: '格式化', sendSelection: '发送', toggleBreakpoint: '断点', gotoDefinition: '定义', findReferences: '引用', rename: '重命名', locale: 'zh' as const,
}

function callbacks() {
  return {
    change: vi.fn<EditorCallbacks['change']>(), view: vi.fn<EditorCallbacks['view']>(), selection: vi.fn<EditorCallbacks['selection']>(),
    problems: vi.fn<EditorCallbacks['problems']>(), languageState: vi.fn<EditorCallbacks['languageState']>(),
    open: vi.fn<EditorCallbacks['open']>(async () => undefined), read: vi.fn<EditorCallbacks['read']>(async () => undefined),
    prepareEdit: vi.fn<EditorCallbacks['prepareEdit']>(async () => undefined), save: vi.fn<EditorCallbacks['save']>(),
    format: vi.fn<EditorCallbacks['format']>(), sendSelection: vi.fn<EditorCallbacks['sendSelection']>(), breakpoint: vi.fn<EditorCallbacks['breakpoint']>(),
  } satisfies EditorCallbacks
}

const document1: EditorDocument = { path: 'main.py', uri: 'untitled:main.py', language: 'python', text: 'a = 1\nb = 2\nprint(a + b)\n', readOnly: false }
let editor: EditorInstance | undefined

afterEach(async () => {
  await editor?.dispose()
  editor = undefined
  document.body.innerHTML = ''
})

function shownView(container: HTMLElement): EditorView {
  const dom = container.querySelector<HTMLElement>('.rainy-editor-surface:not([hidden]) .cm-editor')
  const view = dom === null ? null : EditorView.findFromDOM(dom)
  if (view === null) throw new Error('no visible editor')
  return view
}

it('compares file URIs across drive-letter, colon-encoding and case spellings', () => {
  expect(uriKey('file:///C:/Work/a.py')).toBe(uriKey('file:///c%3A/work/A.py'))
  expect(uriKey('file:///home/u/A.py')).not.toBe(uriKey('file:///home/u/a.py'))
  expect(uriKey('untitled:x')).toBe('untitled:x')
  expect(uriInside('file:///c%3A/Work/src/a.py', 'C:/work')).toBe(true)
  expect(uriInside('file:///home/u/project-2/a.py', '/home/u/project')).toBe(false)
  expect(directoryUri('C:/Work Dir')).toBe('file:///C:/Work%20Dir')
  expect(directoryUri('/home/u/my dir/')).toBe('file:///home/u/my%20dir')
  expect(directoryUri('//server/share')).toBe('file://server/share')
})

it('chooses language support by file name first, then by the IDE language id', () => {
  expect(languageKey('src/App.tsx', 'typescript')).toBe('tsx')
  expect(languageKey('Dockerfile', 'plaintext')).toBe('dockerfile')
  expect(languageKey('exploit.s', 'plaintext')).toBe('x86')
  expect(languageKey('notes.unknown', 'python')).toBe('python')
  expect(languageKey('notes.unknown', 'plaintext')).toBeUndefined()
  expect([lspLanguageId('tsx'), serverLanguage('tsx')]).toEqual(['typescriptreact', 'typescript'])
  expect([lspLanguageId('rust'), serverLanguage('rust')]).toEqual([undefined, undefined])
})

it('strips scripts, handlers and non-web links from server documentation', () => {
  const html = sanitizeDocumentation('<p onclick="x()">Hi <img src=x onerror="alert(1)"><script>alert(2)</script><a href="javascript:alert(3)">j</a> <a href="https://docs.python.org">d</a> <code class="language-py">x</code></p>')
  expect(html).toBe('<p>Hi <a>j</a> <a href="https://docs.python.org" target="_blank" rel="noreferrer">d</a> <code class="language-py">x</code></p>')
})

it('reports user edits but not text the IDE pushes, and keeps one view per document', async () => {
  const container = document.createElement('div')
  document.body.append(container)
  const events = callbacks()
  editor = await assets.create(container, events, labels)
  await editor.setWorkspace({ id: 'w', path: '/project', title: 'project', roots: [] })
  editor.updateDocuments([document1, { ...document1, path: 'b.py', uri: 'untitled:b.py', text: 'x = 1\n' }])
  editor.show('main.py')
  const view = shownView(container)
  expect(view.state.doc.toString()).toBe(document1.text)

  view.dispatch({ changes: { from: 0, insert: '# hi\n' } })
  expect(events.change).toHaveBeenLastCalledWith('main.py', `# hi\n${document1.text}`)

  events.change.mockClear()
  editor.updateDocuments([{ ...document1, text: 'a = 10\nb = 2\nprint(a + b)\n' }])
  expect(shownView(container).state.doc.toString()).toBe('a = 10\nb = 2\nprint(a + b)\n')
  expect(events.change).not.toHaveBeenCalled()
  expect(container.querySelectorAll('.rainy-editor-surface')).toHaveLength(1)

  view.dispatch({ selection: { anchor: 0, head: 6 } })
  expect(events.selection).toHaveBeenLastCalledWith({ path: 'main.py', text: 'a = 10', language: 'python', startLine: 1, startColumn: 1, endLine: 1, endColumn: 7 })
})

it('shows breakpoints and the paused line, and compares against original text', async () => {
  const container = document.createElement('div')
  document.body.append(container)
  editor = await assets.create(container, callbacks(), labels)
  await editor.setWorkspace({ id: 'w', path: '/project', title: 'project', roots: [] })
  editor.updateDocuments([document1])
  editor.show('main.py')
  editor.setBreakpoints([{ path: 'main.py', lines: [2, 3, 99] }], { path: 'main.py', line: 3 })
  const view = shownView(container)
  expect(container.querySelectorAll('.rainy-stopped-line')).toHaveLength(1)
  expect(view.state.doc.lineAt(view.state.selection.main.head).number).toBe(1)

  editor.showDiff('main.py', 'a = 1\nprint(a)\n', false)
  expect(container.querySelector('.cm-deletedChunk, .cm-changedLine, .cm-insertedLine')).not.toBeNull()
  expect(shownView(container).state.readOnly).toBe(true)
  editor.show('main.py')
  expect(container.querySelector('.cm-deletedChunk, .cm-changedLine, .cm-insertedLine')).toBeNull()
  expect(shownView(container).state.readOnly).toBe(false)
  await expect(editor.action('no-such-command')).rejects.toThrow('Unknown editor action')
})
