// @vitest-environment happy-dom
/** Markdown the rich editor keeps as written, with the real Crepe editor and the save pipeline: dollar signs that are not
 *  formulas, reference links and their definitions, Typora's `[TOC]` and `==highlight==`. */
import './standards-mode.ts'
import { describe, expect, it, vi } from 'vitest'
import { Crepe } from '@milkdown/crepe'
import { commandsCtx, editorViewCtx } from '@milkdown/kit/core'
import { imageInlineComponent } from '@milkdown/kit/component/image-inline'
import { updateLinkCommand } from '@milkdown/kit/preset/commonmark'
import type { Node as ProseNode } from '@milkdown/kit/prose/model'
import { NodeSelection, TextSelection } from '@milkdown/kit/prose/state'
import { conventionalTables, mergeEdit } from '../../src/renderer/ide/viewers/markdown-merge.ts'
import { keymapRef } from '@milkdown/crepe/feature/toolbar'
import { HIGHLIGHT_ICON, highlightKeymap, insertToc, markdownReader, markdownSyntax, tocPlugin, toggleHighlightCommand } from '../../src/renderer/ide/viewers/rich-plugins.ts'

const reveal = vi.fn<(index: number) => void>()

async function open(body: string): Promise<Crepe> {
  document.body.replaceChildren()
  const root = document.createElement('div')
  document.body.appendChild(root)
  const crepe = new Crepe({ root, defaultValue: body, features: { [Crepe.Feature.ImageBlock]: false } })
  const editor = await markdownSyntax(crepe.editor, body)
  editor.use(imageInlineComponent).use(tocPlugin({ label: '目录', empty: '没有标题', reveal }))
  await crepe.create()
  return crepe
}

const view = (crepe: Crepe) => crepe.editor.ctx.get(editorViewCtx)

/** Open `text` as RichMarkdown does, apply `edit`, and return what would be saved, and the document a reload shows. */
async function save(text: string, edit: (crepe: Crepe) => void): Promise<{ saved: string; reread: string; written: string; doc: ProseNode }> {
  const crepe = await open(text)
  const reader = markdownReader(crepe.editor.ctx)
  const loaded = conventionalTables(crepe.getMarkdown())
  edit(crepe)
  const written = conventionalTables(crepe.getMarkdown())
  const saved = mergeEdit(text, loaded, written, reader)
  const result = { saved, reread: reader.canonical(saved), written: reader.canonical(written), doc: view(crepe).state.doc }
  await crepe.destroy()
  return result
}

/** @returns Where the first `text` starts in the document text. */
function find(crepe: Crepe, text: string): number {
  let at = -1
  view(crepe).state.doc.descendants((node, position) => {
    if (at < 0 && node.isText && node.text!.includes(text)) at = position + node.text!.indexOf(text)
    return at < 0
  })
  if (at < 0) throw new Error(`not found: ${text}`)
  return at
}

/** Replace the first `from` in the document text, as typing does. */
const type = (from: string, to: string) => (crepe: Crepe): void => {
  const at = find(crepe, from)
  view(crepe).dispatch(view(crepe).state.tr.insertText(to, at, at + from.length))
}

/** @returns The names of the document's nodes, and of the marks on its text, in document order. */
function kinds(doc: ProseNode): string[] {
  const names: string[] = []
  doc.descendants((node) => {
    names.push(node.type.name, ...node.marks.map(mark => mark.type.name))
    return true
  })
  return names
}

function press(target: Element, key: string, modifiers: { ctrlKey?: boolean; shiftKey?: boolean; keyCode?: number } = {}): void {
  target.dispatchEvent(new KeyboardEvent('keydown', { key, bubbles: true, cancelable: true, ...modifiers }))
}

describe('dollar signs', () => {
  const cases: [string, string][] = [
    ['价格 $5 到 $10 EDITME\n', '价格 $5 到 $10 EDITED\n'],
    ['echo $PATH and $HOME EDITME\n', 'echo $PATH and $HOME EDITED\n'],
    ['公式 $x^2$ 和 $5 EDITME\n', '公式 $x^2$ 和 $5 EDITED\n'],
    ['- 列表 $5 到 $10 EDITME\n- 第二项 $3\n', '- 列表 $5 到 $10 EDITED\n- 第二项 $3\n'],
    ['| 价格 | 说明 |\n| --- | --- |\n| $5 | 到 $10 EDITME |\n', '| 价格 | 说明 |\n| --- | --- |\n| $5 | 到 $10 EDITED |\n'],
    ['`$HOME` 与 $5 EDITME\n', '`$HOME` 与 $5 EDITED\n'],
  ]
  for (const [text, expected] of cases) {
    it(`stay bare on an edited line: ${JSON.stringify(text)}`, async () => {
      const { saved, reread, written } = await save(text, type('EDITME', 'EDITED'))
      expect(saved).toBe(expected)
      expect(reread).toBe(written)
    })
  }

  it('make no formula on a reload', async () => {
    const { saved, doc } = await save('价格 $5 到 $10\n', type('$10', '$10 和 $20'))
    expect(saved).toBe('价格 $5 到 $10 和 $20\n')
    expect(kinds(doc)).not.toContain('math_inline')
  })

  it('keep the escape where a reload would read a formula', async () => {
    const { saved, reread, written, doc } = await save('文字\n', type('文字', '文字 $x$ 与 $y'))
    expect(kinds(doc)).not.toContain('math_inline')
    expect(reread).toBe(written)
    expect(saved).not.toMatch(/(?<!\\)\$x(?<!\\)\$/u)
    expect(saved).toContain('$y')
  })

  it('leave formulas formulas', async () => {
    const { saved, doc } = await save('公式 $x^2$ 价格 $5 EDITME\n\n$$\na$b\n$$\n', type('EDITME', 'EDITED'))
    expect(saved).toBe('公式 $x^2$ 价格 $5 EDITED\n\n$$\na$b\n$$\n')
    expect(kinds(doc).filter(kind => kind === 'math_inline')).toHaveLength(1)
  })
})

describe('reference links', () => {
  const text = 'See [文本][ref], [ref][] and [ref] EDITME, ![图][img].\n\n[ref]: https://example.com/a "标题"\n[img]: assets/a.png\n\nAfter\n'

  it('keep their form and their definitions where they are', async () => {
    const { saved, reread, written, doc } = await save(text, type('EDITME', 'EDITED'))
    expect(saved).toBe(text.replace('EDITME', 'EDITED'))
    expect(reread).toBe(written)
    expect(kinds(doc).filter(kind => kind === 'link_definition')).toHaveLength(2)
  })

  it('are written back as references by the editor itself', async () => {
    const crepe = await open(text)
    expect(crepe.getMarkdown()).toBe(text)
    const link = view(crepe).dom.querySelector('a')!
    expect(link.getAttribute('href')).toBe('https://example.com/a')
    expect(link.hasAttribute('reference')).toBe(false)
    await crepe.destroy()
  })

  it('keep the definition when only the text before it changes', async () => {
    const doc = 'A [x][1] EDITME\n\n[1]: http://a\n\nB\n\n[2]: http://b\n'
    expect((await save(doc, type('EDITME', 'EDITED'))).saved).toBe(doc.replace('EDITME', 'EDITED'))
    expect((await save(doc, type('B', 'BB'))).saved).toBe(doc.replace('B\n', 'BB\n'))
  })

  it('become inline links when given another target', async () => {
    const { saved } = await save(text, (crepe) => {
      const at = find(crepe, '文本')
      view(crepe).dispatch(view(crepe).state.tr.setSelection(TextSelection.create(view(crepe).state.doc, at + 1)))
      crepe.editor.ctx.get(commandsCtx).call(updateLinkCommand.key, { href: 'https://example.com/b' })
    })
    expect(saved).toContain('See [文本](https://example.com/b "标题"), [ref][] and [ref]')
    expect(saved).toContain('[ref]: https://example.com/a "标题"\n')
  })

  it('become inline links when their definition is deleted', async () => {
    const { saved, reread, written } = await save(text, (crepe) => {
      let at = -1
      view(crepe).state.doc.descendants((node, position) => {
        if (at < 0 && node.type.name === 'link_definition' && node.attrs.label === 'ref') at = position
        return at < 0
      })
      view(crepe).dispatch(view(crepe).state.tr.setSelection(NodeSelection.create(view(crepe).state.doc, at)).deleteSelection())
    })
    expect(saved).toBe('See [文本](https://example.com/a "标题"), [ref](https://example.com/a "标题") and [ref](https://example.com/a "标题") EDITME, ![图][img].\n\n[img]: assets/a.png\n\nAfter\n')
    expect(reread).toBe(written)
  })
})

describe('[TOC]', () => {
  const text = '[TOC]\n\n# 第一章 EDITME\n\n## 小节\n\n# 第二章\n'

  it('is a table of contents written back as [TOC]', async () => {
    const { saved, reread, written, doc } = await save(text, type('EDITME', 'EDITED'))
    expect(saved).toBe(text.replace('EDITME', 'EDITED'))
    expect(reread).toBe(written)
    expect(doc.firstChild?.type.name).toBe('toc')
    const crepe = await open('[toc]\n\nText\n')
    expect(crepe.getMarkdown()).toBe('[toc]\n\nText\n')
    await crepe.destroy()
  })

  it('lists the headings nested, follows their changes and scrolls to the one clicked', async () => {
    const crepe = await open(text)
    const toc = (): HTMLElement => view(crepe).dom.querySelector<HTMLElement>('.rainy-toc')!
    expect(toc().querySelector('.rainy-toc-title')?.textContent).toBe('目录')
    expect([...toc().querySelectorAll(':scope > ul > li > a')].map(link => link.textContent)).toEqual(['第一章 EDITME', '第二章'])
    expect(toc().querySelector(':scope > ul > li > ul > li > a')?.textContent).toBe('小节')
    type('小节', '新小节')(crepe)
    expect(toc().querySelector(':scope > ul > li > ul > li > a')?.textContent).toBe('新小节')
    reveal.mockClear()
    toc().querySelectorAll('a')[2]!.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }))
    expect(reveal).toHaveBeenCalledWith(2)
    await crepe.destroy()
  })

  it('is made by Enter after [TOC], as in Typora', async () => {
    const crepe = await open('# 标题\n\nX\n')
    type('X', '[TOC]')(crepe)
    const at = find(crepe, '[TOC]') + '[TOC]'.length
    view(crepe).dispatch(view(crepe).state.tr.setSelection(TextSelection.create(view(crepe).state.doc, at)))
    press(view(crepe).dom, 'Enter')
    expect(view(crepe).state.doc.child(1).type.name).toBe('toc')
    expect(crepe.getMarkdown()).toBe('# 标题\n\n[TOC]\n')
    await crepe.destroy()
  })

  it('is inserted from the / menu after the current block', async () => {
    const crepe = await open('A\n')
    view(crepe).dispatch(view(crepe).state.tr.setSelection(TextSelection.create(view(crepe).state.doc, 2)))
    expect(insertToc(crepe.editor.ctx)).toBe(true)
    expect(crepe.getMarkdown()).toBe('A\n\n[TOC]\n')
    await crepe.destroy()
  })

  it('stays text when written \\[TOC], with other text or in a list', async () => {
    for (const [doc, written] of [['\\[TOC]\n', '\\[TOC]\n'], ['[TOC] 目录\n', '\\[TOC] 目录\n'], ['- [TOC]\n', '- \\[TOC]\n']] as const) {
      const crepe = await open(doc)
      expect(kinds(view(crepe).state.doc)).not.toContain('toc')
      expect(crepe.getMarkdown()).toBe(written)
      await crepe.destroy()
    }
  })
})

describe('==highlight==', () => {
  it('is a highlight written back as ==text==', async () => {
    const { saved, doc } = await save('==高亮== 与 ==两处== EDITME\n', type('EDITME', 'EDITED'))
    expect(saved).toBe('==高亮== 与 ==两处== EDITED\n')
    expect(kinds(doc).filter(kind => kind === 'highlight')).toHaveLength(2)
    const crepe = await open('==高亮==\n')
    expect(view(crepe).dom.querySelector('mark')?.textContent).toBe('高亮')
    await crepe.destroy()
  })

  it('is toggled with Ctrl+Shift+H', async () => {
    const { saved } = await save('一个 重点 词\n', (crepe) => {
      const at = find(crepe, '重点')
      view(crepe).dispatch(view(crepe).state.tr.setSelection(TextSelection.create(view(crepe).state.doc, at, at + 2)))
      press(view(crepe).dom, 'H', { ctrlKey: true, shiftKey: true, keyCode: 72 })
    })
    expect(saved).toBe('一个 ==重点== 词\n')
  })

  it('has a toolbar button named 高亮 with its shortcut', async () => {
    document.body.replaceChildren()
    const root = document.createElement('div')
    document.body.appendChild(root)
    const crepe = new Crepe({ root, defaultValue: '一个 重点 词\n', features: { [Crepe.Feature.ImageBlock]: false }, featureConfigs: {
      [Crepe.Feature.Toolbar]: { buildToolbar: (builder) => {
        builder.getGroup('formatting').addItem('highlight', { icon: HIGHLIGHT_ICON, label: '高亮', keymap: keymapRef(highlightKeymap.key, 'ToggleHighlight'),
          active: () => false, onRun: (ctx) => { ctx.get(commandsCtx).call(toggleHighlightCommand.key) } })
      } },
    } })
    await markdownSyntax(crepe.editor, '一个 重点 词\n')
    await crepe.create()
    const at = find(crepe, '重点')
    view(crepe).dispatch(view(crepe).state.tr.setSelection(TextSelection.create(view(crepe).state.doc, at, at + 2)))
    await new Promise(resolve => setTimeout(resolve, 60))
    const button = document.querySelector<HTMLElement>('[data-toolbar-item="highlight"]')!
    expect(button.getAttribute('title')).toMatch(/^高亮 \(.*Shift.*H\)$/u)
    button.dispatchEvent(new PointerEvent('pointerdown', { bubbles: true, cancelable: true }))
    expect(crepe.getMarkdown()).toBe('一个 ==重点== 词\n')
    await crepe.destroy()
  })

  it('is made by typing ==text==', async () => {
    const crepe = await open('起点\n')
    const editor = view(crepe)
    const at = find(crepe, '起点') + 2
    editor.dispatch(editor.state.tr.insertText(' ==标记=', at))
    const end = at + ' ==标记='.length
    const handled = editor.someProp('handleTextInput', handle => handle(editor, end, end, '=', () => editor.state.tr.insertText('=', end, end)))
    expect(handled).toBe(true)
    expect(crepe.getMarkdown()).toBe('起点 ==标记==\n')
    expect(kinds(editor.state.doc)).toContain('highlight')
    await crepe.destroy()
  })

  it('leaves setext underlines, comparisons and code alone', async () => {
    const text = 'Title\n=====\n\na == b 与 `x == y` 和 a===b EDITME\n\n```js\nif (a == b) {}\n```\n'
    const { saved, doc } = await save(text, type('EDITME', 'EDITED'))
    expect(saved).toBe(text.replace('EDITME', 'EDITED'))
    expect(kinds(doc)).not.toContain('highlight')
  })
})
