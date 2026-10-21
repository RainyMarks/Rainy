// @vitest-environment happy-dom
/** Find in the rich editor, where pasted images go, and the exported HTML page. */
import { describe, expect, it, vi } from 'vitest'
import { Schema } from '@milkdown/kit/prose/model'
import { findMatches } from '../../src/renderer/ide/viewers/rich-plugins.ts'
import { imageFileName, imageFolder, markdownImageTarget, saveMarkdownImage } from '../../src/renderer/ide/viewers/markdown-assets.ts'
import { documentTitle, exportHtml } from '../../src/renderer/ide/viewers/markdown-export.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
vi.mock('../../src/renderer/ide/viewers/mermaid.ts', () => ({
  isMermaid: (language: string | undefined) => language === 'mermaid',
  renderMermaid: async (source: string) => `<svg data-test="diagram">${source.length}</svg>`,
}))

const schema = new Schema({
  nodes: {
    doc: { content: 'block+' },
    paragraph: { group: 'block', content: 'inline*', toDOM: () => ['p', 0] },
    code_block: { group: 'block', content: 'text*', code: true, toDOM: () => ['pre', ['code', 0]] },
    image: { group: 'inline', inline: true, attrs: { src: { default: '' } }, toDOM: node => ['img', { src: node.attrs.src as string }] },
    text: { group: 'inline' },
  },
  marks: { strong: { toDOM: () => ['strong', 0] } },
})

describe('find in the rich editor', () => {
  const strong = schema.marks.strong!.create()
  const doc = schema.node('doc', null, [
    schema.node('paragraph', null, [schema.text('Rainy '), schema.text('rai', [strong]), schema.text('ny day')]),
    schema.node('code_block', null, [schema.text('rainy in code')]),
    schema.node('paragraph', null, [schema.text('RAIN'), schema.node('image', { src: 'a.png' }), schema.text('ny')]),
  ])

  it('matches across marks, ignores case unless asked, and skips code blocks', () => {
    const matches = findMatches(doc, 'rainy', false)
    expect(matches.map(match => doc.textBetween(match.from, match.to))).toEqual(['Rainy', 'rainy'])
    expect(findMatches(doc, 'Rainy', true)).toHaveLength(1)
    // A match never runs through an inline image.
    expect(findMatches(doc, 'rainny', false)).toEqual([])
    expect(findMatches(doc, '', false)).toEqual([])
    // Regular expression characters are literal.
    expect(findMatches(doc, 'a.', false)).toEqual([])
  })
})

describe('pasted images', () => {
  it('saves into assets, <name>.assets or the document folder, keeping the mounted root', () => {
    expect(imageFolder('notes/day.md', 'assets')).toEqual({ folder: 'notes/assets', relative: 'assets' })
    expect(imageFolder('day.md', 'named')).toEqual({ folder: 'day.assets', relative: 'day.assets' })
    expect(imageFolder('notes/day.md', 'same')).toEqual({ folder: 'notes', relative: '' })
    expect(imageFolder('\0r1/day.md', 'assets').folder).toBe('\0r1/assets')
  })

  it('names screenshots by time and keeps the names of dropped files', () => {
    const now = new Date(2026, 9, 20, 9, 5, 7, 42)
    expect(imageFileName({ name: 'image.png', type: 'image/png' }, now)).toBe('image-20261020090507042.png')
    expect(imageFileName({ name: '', type: 'image/jpeg' }, now)).toBe('image-20261020090507042.jpg')
    expect(imageFileName({ name: '实验结果 (1).PNG', type: 'image/png' }, now)).toBe('实验结果 (1).png')
    expect(imageFileName({ name: 'a:b?.webp', type: 'image/webp' }, now)).toBe('a-b-.webp')
    expect(markdownImageTarget('assets', '实验结果 (1).png')).toBe('assets/实验结果%20%281%29.png')
  })

  it('returns the target of the file actually created', async () => {
    const save = vi.fn(async (folder: string, name: string) => `${folder}/${name.replace('.png', '-1.png')}`)
    const file = new File([new Uint8Array([1, 2, 3])], 'chart.png', { type: 'image/png' })
    await expect(saveMarkdownImage(save, 'docs/report.md', file, 'assets')).resolves.toBe('assets/chart-1.png')
    expect(save).toHaveBeenCalledWith('docs/assets', 'chart.png', new Uint8Array([1, 2, 3]))
  })
})

describe('export', () => {
  it('builds one page with highlighted code, MathML, embedded images and diagrams, titled by the front matter', async () => {
    const text = '---\ntitle: 实验报告\n---\n\n# Results\n\nInline $E=mc^2$ and ![chart](assets/chart.png) and ![web](https://example.com/a.png).\n\n'
      + '```python\nprint("hi")\n```\n\n```mermaid\ngraph TD\nA-->B\n```\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n\n- [x] done\n'
    const image = vi.fn(async (target: string) => target.startsWith('http') ? undefined : 'data:image/png;base64,AAAA')
    const html = await exportHtml({ text: `${text}\nNote[^1].\n\n[^1]: 注释。\n`, name: 'report', image })
    expect(html).toMatch(/^<!DOCTYPE html>/u)
    expect(html).toContain('<html lang="zh-CN">')
    expect(html).toContain('脚注')
    expect(html).toContain('class="task-list-item"')
    expect(html).toContain('<title>实验报告</title>')
    expect(html).toContain('<math')
    expect(html).not.toContain('katex-html')
    expect(html).toContain('src="data:image/png;base64,AAAA"')
    expect(html).toContain('src="https://example.com/a.png"')
    expect(html).toContain('<svg data-test="diagram">')
    expect(html).not.toContain('language-mermaid')
    // Front matter is metadata: it titles the page and is not printed.
    expect(html).not.toContain('title: 实验报告')
    expect(html).toContain('<table>')
    expect(html).toMatch(/hljs-built_in|hljs-string/u)
    expect(html).not.toMatch(/<script/iu)
  })

  it('titles a page by its front matter, then its first heading, then its file name', () => {
    expect(documentTitle('---\ntitle: "A"\n---\n# B\n', 'c')).toBe('A')
    expect(documentTitle('Intro\n\n## B *bold*\n', 'c')).toBe('B bold')
    expect(documentTitle('no headings', 'c')).toBe('c')
  })
})
