// @vitest-environment happy-dom
/** Editing in the rich Markdown editor with the real Crepe: Typora's Enter in tables, outside changes Ctrl+Z cannot
 *  revert, and code blocks in Rainy's colors without CodeMirror's English find panel. */
import './standards-mode.ts'
import { describe, expect, it } from 'vitest'
import { Crepe } from '@milkdown/crepe'
import { editorViewCtx } from '@milkdown/kit/core'
import { TextSelection } from '@milkdown/kit/prose/state'
import { replaceAll } from '@milkdown/kit/utils'
import { codeTheme } from '../../src/renderer/ide/viewers/code-theme.ts'
import { viewerMessages } from '../../src/renderer/ide/viewers/messages.ts'
import { markdownSyntax, tableKeys, typoraKeymap } from '../../src/renderer/ide/viewers/rich-plugins.ts'

// Code blocks start CodeMirror once they scroll into view; here every block is in view.
globalThis.IntersectionObserver = class {
  constructor(private readonly callback: IntersectionObserverCallback) {}
  observe(target: Element): void { this.callback([{ target, isIntersecting: true } as IntersectionObserverEntry], this as unknown as IntersectionObserver) }
  unobserve(): void {}
  disconnect(): void {}
  takeRecords(): IntersectionObserverEntry[] { return [] }
} as unknown as typeof IntersectionObserver

async function open(markdown: string): Promise<Crepe> {
  document.body.replaceChildren()
  const root = document.createElement('div')
  document.body.appendChild(root)
  const crepe = new Crepe({ root, defaultValue: markdown, features: { [Crepe.Feature.ImageBlock]: false },
    featureConfigs: { [Crepe.Feature.CodeMirror]: { theme: codeTheme(viewerMessages.t) } } })
  const editor = await markdownSyntax(crepe.editor, markdown)
  editor.use(typoraKeymap).use(tableKeys)
  await crepe.create()
  // Code blocks are Vue node views, drawn a tick later.
  await new Promise(resolve => setTimeout(resolve, 20))
  return crepe
}

function press(target: Element, key: string, modifiers: { ctrlKey?: boolean; shiftKey?: boolean; keyCode?: number } = {}): void {
  target.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...modifiers }))
}

/** Put the cursor at the end of the first text containing `text`. */
function cursorAfter(crepe: Crepe, text: string): void {
  const view = crepe.editor.ctx.get(editorViewCtx)
  let at = -1
  view.state.doc.descendants((node, position) => {
    if (at < 0 && node.isText && node.text!.includes(text)) at = position + node.text!.indexOf(text) + text.length
    return at < 0
  })
  view.dispatch(view.state.tr.setSelection(TextSelection.create(view.state.doc, at)))
}

describe('Enter in a table', () => {
  const table = '| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n'

  it('goes to the cell below, and adds a row after the last one', async () => {
    const crepe = await open(table)
    const view = crepe.editor.ctx.get(editorViewCtx)
    cursorAfter(crepe, '2')
    press(view.dom, 'Enter')
    view.dispatch(view.state.tr.insertText('X'))
    expect(crepe.getMarkdown()).toContain('| 3 | 4X |')
    press(view.dom, 'Enter')
    view.dispatch(view.state.tr.insertText('Y'))
    expect(crepe.getMarkdown()).toMatch(/\| 3 \| 4X \|\n\| +\| Y \|/u)
    await crepe.destroy()
  })

  it('adds a row below with Ctrl+Enter', async () => {
    const crepe = await open(table)
    const view = crepe.editor.ctx.get(editorViewCtx)
    cursorAfter(crepe, '1')
    press(view.dom, 'Enter', { ctrlKey: true })
    view.dispatch(view.state.tr.insertText('N'))
    expect(crepe.getMarkdown()).toMatch(/\| 1 \| 2 \|\n\| N \| +\|\n\| 3 \| 4 \|/u)
    await crepe.destroy()
  })

  it('still splits paragraphs outside tables', async () => {
    const crepe = await open('One two\n')
    const view = crepe.editor.ctx.get(editorViewCtx)
    cursorAfter(crepe, 'One')
    press(view.dom, 'Enter')
    expect(view.state.doc.childCount).toBe(2)
    await crepe.destroy()
  })
})

it('inserts an image to fill in with Ctrl+Shift+I', async () => {
  const crepe = await open('Text\n')
  const view = crepe.editor.ctx.get(editorViewCtx)
  cursorAfter(crepe, 'Text')
  press(view.dom, 'I', { ctrlKey: true, shiftKey: true, keyCode: 73 })
  let images = 0
  view.state.doc.descendants((node) => { if (node.type.name === 'image') images++ })
  expect(images).toBe(1)
  await crepe.destroy()
})

it('cannot undo a change that came from outside', async () => {
  const crepe = await open('First\n')
  const view = crepe.editor.ctx.get(editorViewCtx)
  cursorAfter(crepe, 'First')
  view.dispatch(view.state.tr.insertText(' typed'))
  crepe.editor.action(replaceAll('Written by the agent\n', true))
  press(crepe.editor.ctx.get(editorViewCtx).dom, 'z', { ctrlKey: true })
  expect(crepe.getMarkdown()).toBe('Written by the agent\n')
  await crepe.destroy()
})

describe('code blocks', () => {
  it('highlight with Rainy token classes and keep Ctrl+F for the find bar', async () => {
    const crepe = await open('```js\nconst x = 1 // note\n```\n')
    const content = document.querySelector('.milkdown-code-block .cm-content')!
    expect(content.querySelector('.tok-keyword')?.textContent).toBe('const')
    expect(content.querySelector('.tok-comment')).not.toBeNull()
    press(content, 'f', { ctrlKey: true })
    expect(document.querySelector('.cm-search')).toBeNull()
    await crepe.destroy()
  })

  it('show a ```latex fence as code and $$ as a formula', async () => {
    const crepe = await open('```latex\n\\documentclass{article}\n```\n\n$$\nx^2\n$$\n')
    const languages = [...document.querySelectorAll('.milkdown-code-block .language-button')].map(button => button.textContent?.trim())
    expect(languages).toEqual(['latex', 'LaTeX'])
    await crepe.destroy()
  })
})
