/** Front matter, the outline and word counts of Markdown documents. */
import { describe, expect, it } from 'vitest'
import { frontMatterContent, splitFrontMatter, withFrontMatterContent } from '../../src/renderer/ide/viewers/front-matter.ts'
import { headingAnchors, headingForFragment, markdownOutline, plainHeading } from '../../src/renderer/ide/viewers/markdown-outline.ts'
import { countWords } from '../../src/renderer/ide/viewers/word-count.ts'

describe('front matter', () => {
  it('splits YAML and TOML blocks at the start of the file only', () => {
    expect(splitFrontMatter('---\ntitle: A\ntags: [x]\n---\n\n# A\n')).toEqual({ front: '---\ntitle: A\ntags: [x]\n---\n', body: '\n# A\n' })
    expect(splitFrontMatter('+++\ntitle = "A"\n+++\nText')).toEqual({ front: '+++\ntitle = "A"\n+++\n', body: 'Text' })
    expect(splitFrontMatter('---\na: 1\n...\nText').front).toBe('---\na: 1\n...\n')
    expect(splitFrontMatter('---\n---\nText')).toEqual({ front: '---\n---\n', body: 'Text' })
    for (const text of ['# A\n---\nb: 1\n---\n', 'Text\n', '---\nnot closed\n']) expect(splitFrontMatter(text)).toEqual({ front: '', body: text })
  })

  it('edits the lines between the delimiters and keeps the delimiters', () => {
    const front = '---\ntitle: A\n---\n'
    expect(frontMatterContent(front)).toBe('title: A')
    expect(withFrontMatterContent(front, 'title: B\nlang: zh\n')).toBe('---\ntitle: B\nlang: zh\n---\n')
    expect(withFrontMatterContent('---\na: 1\n...\n', '')).toBe('---\n...\n')
  })
})

describe('outline', () => {
  const text = [
    '---', 'title: Not a heading', '---', '', '# 第一章 **重点**', 'Intro', '', 'Setext title', '============', '', '```md', '# code, not a heading', '```',
    '', '$$', '# math', '$$', '', '> ## Quoted [link](x.md)', '', '- item', '---', '', 'Para line', 'continues', '---', '', '### Closing ###', '#Not a heading',
  ].join('\n')

  it('finds ATX, setext and quoted headings outside code, math and front matter', () => {
    expect(markdownOutline(text)).toEqual([
      { level: 1, text: '第一章 重点', line: 5 },
      { level: 1, text: 'Setext title', line: 8 },
      { level: 2, text: 'Quoted link', line: 19 },
      { level: 2, text: 'Para line continues', line: 24 },
      { level: 3, text: 'Closing', line: 28 },
    ])
  })

  it('strips inline syntax and makes GitHub anchors with numbered repeats', () => {
    expect(plainHeading('Use `code`, *emphasis* and ![img](a.png) in snake_case_name')).toBe('Use code, emphasis and img in snake_case_name')
    const headings = [{ text: '列表' }, { text: 'Hello World!' }, { text: '列表' }]
    expect(headingAnchors(headings)).toEqual(['列表', 'hello-world', '列表-1'])
    expect(headingForFragment(headings, '%E5%88%97%E8%A1%A8-1')).toBe(2)
    expect(headingForFragment(headings, 'Hello World')).toBe(1)
    expect(headingForFragment(headings, 'missing')).toBe(-1)
  })
})

describe('word count', () => {
  it('counts CJK characters and other words without syntax, targets or front matter', () => {
    const count = countWords('---\ntitle: skip me\n---\n# 标题\n\n这是**一段**文字，with [a link](https://example.com/very/long) and `code`.\n\n- [x] 完成 task\n')
    // 标题 这是一段文字 完成 = 10 characters; with a link and code task = 6 words.
    expect(count.words).toBe(16)
    expect(count.minutes).toBe(1)
    expect(countWords('').words).toBe(0)
    expect(countWords('| a | b |\n| --- | --- |\n| 1 | 2 |\n').words).toBe(4)
  })
})
