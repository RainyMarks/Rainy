// @vitest-environment happy-dom
/** The rich editor's load → edit → save pipeline with the real Crepe editor: an edit never loses or duplicates text, and
 *  the lines it did not touch keep the file's spelling and line ends. */
import './standards-mode.ts'
import { describe, expect, it } from 'vitest'
import { Crepe } from '@milkdown/crepe'
import { editorViewCtx } from '@milkdown/kit/core'
import { imageInlineComponent } from '@milkdown/kit/component/image-inline'
import { conventionalTables, mergeEdit } from '../../src/renderer/ide/viewers/markdown-merge.ts'
import { splitFrontMatter } from '../../src/renderer/ide/viewers/front-matter.ts'
import { markdownReader, markdownSyntax } from '../../src/renderer/ide/viewers/rich-plugins.ts'


const lf = (text: string): string => text.replace(/\r\n?/gu, '\n')

async function open(body: string): Promise<Crepe> {
  const root = document.createElement('div')
  document.body.appendChild(root)
  const crepe = new Crepe({ root, defaultValue: lf(body), features: { [Crepe.Feature.ImageBlock]: false } })
  const editor = await markdownSyntax(crepe.editor, lf(body))
  editor.use(imageInlineComponent)
  await crepe.create()
  return crepe
}

/** Positions at the start of every non-empty text block outside code. */
function textBlocks(crepe: Crepe): number[] {
  const view = crepe.editor.ctx.get(editorViewCtx)
  const starts: number[] = []
  view.state.doc.descendants((node, position) => {
    if (node.isTextblock && node.type.spec.code !== true && node.textContent !== '') starts.push(position + 1)
    return true
  })
  return starts
}

/** Open `text` as RichMarkdown does, apply `edit`, and return what would be saved and what the editor held. */
async function save(text: string, edit: (crepe: Crepe) => void): Promise<{ saved: string; written: string; reread: string }> {
  const { front, body } = splitFrontMatter(text)
  const crepe = await open(body)
  const reader = markdownReader(crepe.editor.ctx)
  const loaded = conventionalTables(crepe.getMarkdown())
  edit(crepe)
  const written = conventionalTables(crepe.getMarkdown())
  const saved = front + mergeEdit(body, loaded, written, reader)
  const reread = reader.canonical(splitFrontMatter(saved).body)
  const canonical = reader.canonical(written)
  await crepe.destroy()
  return { saved, written: canonical, reread }
}

/** Replace the first `from` in the document text, as typing does. */
const type = (from: string, to: string) => (crepe: Crepe): void => {
  const view = crepe.editor.ctx.get(editorViewCtx)
  let at = -1
  view.state.doc.descendants((node, position) => {
    if (at < 0 && node.isText && node.text!.includes(from)) at = position + node.text!.indexOf(from)
    return at < 0
  })
  if (at < 0) throw new Error(`not found: ${from}`)
  view.dispatch(view.state.tr.insertText(to, at, at + from.length))
}

/** The audit's documents: lists the editor loosens, mixed markers, comments, tables, references, Typora syntax. */
const DOCUMENTS: Record<string, string> = {
  bullets: '* one\n* two\n\n* three\n\n\n\nPara A\n\n* four\n* five\n\nPara B\n',
  // The file's main marker is `-`, so the `*` lists are written back with `-`, loosened and with fewer blank lines.
  bulletsMixed: '- w\n- x\n- y\n- z\n\nPara\n\n* one\n* two\n\n* three\n\n\n\nPara A\n',
  listsAdjacent: '- a\n- b\n\n* c\n* d\n\n- e\n\n\n- f\n',
  looseOrdered: '1. a\n2. b\n\n3. c\n\n\nText\n\n1. d\n',
  comments: '<!-- a -->\nPara A\n<!-- b -->\nPara B\n\n<!-- c -->\nPara C\n',
  tables: '|a|b|\n|:-|-:|\n|1|2|\n\nPara A\n\n|c|d|\n|-|-|\n|3|4|\n\nPara B\n',
  emphasis: '__x__ one\n\n__y__ two\n\n_z_ three\n\n__w__ four\n',
  refs: '[a][1] one\n\n[b][2] two\n\n[1]: http://a\n[2]: http://b\n\nPara C\n',
  mixed: '# T\n\n* a\n* b\n\n1) x\n2) y\n\nSetext\n===\n\n***\n\n+ p\n+ q\n\nEnd\n',
  latexFence: '```latex\n\\documentclass{article}\n\\begin{document}x\\end{document}\n```\n\nText\n',
  htmlBlock: '<div align="center">\n<img src="a.png" width="300">\n</div>\n\nText\n',
  inlineHtml: '第一行<br>第二行 <sup>2</sup>\n',
  footnote: 'Note[^1] text.\n\n[^1]: 注释。\n',
  task: '- [ ] todo\n- [x] done\n',
  nested: '- parent\n    - child\n    - child2\n',
  ordered: '1) one\n2) two\n',
  imageTitle: '![图](a.png "标题") text\n',
  escapes: '1\\. 不是列表 \\*星号\\* a_b_c ~/x 50%\n',
  hardBreak: '第一行  \n第二行\n',
  softWrap: '很长的一段中文\n在这里换行\n',
  subscript: 'H~2~O and ~/home\n',
  highlight: '==高亮== text\n',
  toc: '[TOC]\n\n# A\n',
  setext: 'Title\n=====\n\nBody\n',
  tablePipe: '| a | b |\n|---|---|\n| x \\| y | z |\n',
  autolink: '<https://example.com> text\n',
  emptyLines: 'A\n\n\n\nB\n',
  math: '$$\nx^2\n$$\n\n价格 $5 到 $10，公式 $x$。\n',
  crlf: '# 标题\r\n\r\n* 星号列表\r\n* 第二项\r\n\r\n__粗体__ 和 _斜体_\r\n\r\n段落。\r\n',
}

describe('an edit keeps the document', () => {
  it('saves what the editor holds, whatever block is edited', async () => {
    const failures: string[] = []
    for (const [name, text] of Object.entries(DOCUMENTS)) {
      const probe = await open(text)
      const count = textBlocks(probe).length
      await probe.destroy()
      for (let index = 0; index < count; index++) {
        const { saved, written, reread } = await save(text, (crepe) => {
          const view = crepe.editor.ctx.get(editorViewCtx)
          view.dispatch(view.state.tr.insertText('Z', textBlocks(crepe)[index]!))
        })
        if (reread !== written) failures.push(`${name} #${index}: ${JSON.stringify(saved)} reads ${JSON.stringify(reread)}, not ${JSON.stringify(written)}`)
      }
    }
    expect(failures).toEqual([])
  })

  it('keeps the lines it did not touch', async () => {
    const text = DOCUMENTS.bullets!
    expect((await save(text, type('Para A', 'Para AZ'))).saved).toBe(text.replace('Para A', 'Para AZ'))
    expect((await save(text, type('one', 'oneZ'))).saved).toContain('Para A\n\n* four\n* five\n\nPara B\n')
    // A list the line merge cannot place is taken whole from the editor; the blocks around it stay as written.
    const { saved } = await save(DOCUMENTS.bulletsMixed!, type('one', 'oneZ'))
    expect(saved).toMatch(/^- w\n- x\n- y\n- z\n\nPara\n\n[-*] oneZ\n/u)
    expect(saved).toMatch(/three\n\nPara A\n$/u)
  })
})

describe('CRLF files', () => {
  const text = '---\r\ntitle: 报告\r\ntags: [a, b]\r\n---\r\n\r\n# 标题\r\n\r\n* 星号列表\r\n* 第二项\r\n\r\n__粗体__ 和 _斜体_ a_b_c\r\n\r\n段落 EDITME。\r\n'

  it('split their front matter', () => {
    expect(splitFrontMatter(text).front).toBe('---\r\ntitle: 报告\r\ntags: [a, b]\r\n---\r\n')
  })

  it('change only the edited line and keep CRLF', async () => {
    expect((await save(text, type('EDITME', 'EDITED'))).saved).toBe(text.replace('EDITME', 'EDITED'))
  })

  it('give new lines the file line end', async () => {
    const { saved } = await save('A\r\n\r\nB\r\n', (crepe) => {
      const view = crepe.editor.ctx.get(editorViewCtx)
      const after = view.state.doc.firstChild!.nodeSize
      view.dispatch(view.state.tr.insert(after, view.state.schema.nodes.paragraph!.create(null, view.state.schema.text('New'))))
    })
    expect(saved).toBe('A\r\n\r\nNew\r\n\r\nB\r\n')
  })

  it('keep the line ends of a mixed file outside the edit', async () => {
    expect((await save('A\r\n\r\nB\n\nC EDITME\r\n', type('EDITME', 'EDITED'))).saved).toBe('A\r\n\r\nB\n\nC EDITED\r\n')
  })
})

describe('an edited line', () => {
  const cases: [string, string, string][] = [
    ['keeps lone tildes', 'H~2~O ~/x EDITME\n', 'H~2~O ~/x EDITED\n'],
    ['keeps a highlight', '==高亮== EDITME\n', '==高亮== EDITED\n'],
    ['keeps a latex fence', '```latex\n\\begin{document}\n```\n\nEDITME\n', '```latex\n\\begin{document}\n```\n\nEDITED\n'],
    ['keeps ordered markers with parentheses', '1) one EDITME\n2) two\n', '1) one EDITED\n2) two\n'],
    ['keeps star bullets', '* one EDITME\n* two\n', '* one EDITED\n* two\n'],
    ['keeps dollar amounts as text', '价格 $5 到 $10 EDITME\n', '价格 \\$5 到 \\$10 EDITED\n'],
  ]
  for (const [name, text, expected] of cases) {
    it(name, async () => { expect((await save(text, type('EDITME', 'EDITED'))).saved).toBe(expected) })
  }

  it('leaves a latex fence a code block and $$ a formula', async () => {
    const text = '```latex\nA\n```\n\n$$\nx^2\n$$\n'
    expect((await save(text, (crepe) => {
      const view = crepe.editor.ctx.get(editorViewCtx)
      view.dispatch(view.state.tr.insertText('Z', 1))
    })).saved).toBe('```latex\nZA\n```\n\n$$\nx^2\n$$\n')
  })

  it('writes no <br /> for empty paragraphs', async () => {
    const { saved } = await save('A\n\nB\n', (crepe) => {
      const view = crepe.editor.ctx.get(editorViewCtx)
      const end = view.state.doc.firstChild!.nodeSize - 1
      view.dispatch(view.state.tr.split(end))
      view.dispatch(view.state.tr.split(view.state.selection.from))
    })
    expect(saved).not.toContain('<br')
  })
})
