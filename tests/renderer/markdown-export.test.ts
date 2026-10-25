// @vitest-environment happy-dom
/** The exported page of a Markdown file: raw HTML through the allowlist, highlights and dollar amounts. */
import { describe, expect, it, vi } from 'vitest'
import { exportHtml } from '../../src/renderer/ide/viewers/markdown-export.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
vi.mock('../../src/renderer/ide/viewers/mermaid.ts', () => ({
  isMermaid: (language: string | undefined) => language === 'mermaid',
  renderMermaid: async (source: string) => `<svg data-test="diagram">${source.length}</svg>`,
}))

Object.defineProperty(document, 'compatMode', { value: 'CSS1Compat', configurable: true })

/** @returns The `<main>` of the exported page. */
async function exported(text: string, image = async (_target: string): Promise<string | undefined> => 'data:image/png;base64,AAAA'): Promise<string> {
  const html = await exportHtml({ text, name: 'report', image })
  return html.slice(html.indexOf('<main>'), html.indexOf('</main>'))
}

describe('export of a Markdown file', () => {
  it('never prints HTML comments', async () => {
    const main = await exported('# 结果\n\n<!-- 内部备注：不要外传 -->\n\n段落 <!-- 行内备注 --> 继续\n\n<!--\n多行\n\n备注\n-->\n\n结尾\n')
    expect(main).not.toMatch(/备注|不要外传|&lt;!--/u)
    expect(main).toContain('段落')
    expect(main).toContain('结尾')
  })

  it('renders Typora images with their zoom, embedded, and inline HTML as elements', async () => {
    const image = vi.fn(async () => 'data:image/png;base64,AAAA')
    const main = await exported('<img src="assets/a.png" style="zoom:50%;" />\n\n第一行<br>第二行 H<sub>2</sub>O，x<sup>2</sup>，<kbd>Ctrl</kbd>\n', image)
    expect(main).toMatch(/<img src="data:image\/png;base64,AAAA" style="zoom:\s*50%;?"/u)
    expect(image).toHaveBeenCalledWith('assets/a.png')
    expect(main).toContain('第一行<br>第二行 H<sub>2</sub>O')
    expect(main).toContain('<sup>2</sup>')
    expect(main).toContain('<kbd>Ctrl</kbd>')
    expect(main).not.toContain('&lt;')
  })

  it('keeps folded sections around Markdown and centered blocks', async () => {
    const main = await exported('<details>\n<summary>展开</summary>\n\n内容 **粗体**\n\n</details>\n\n<div align="center">\n<img src="logo.png" width="200">\n</div>\n')
    expect(main).toMatch(/<details>\s*<summary>展开<\/summary>\s*<p>内容 <strong>粗体<\/strong><\/p>\s*<\/details>/u)
    expect(main).toMatch(/<div align="center">\s*<img src="data:image\/png;base64,AAAA" width="200"\/?>\s*<\/div>/u)
  })

  it('drops scripts, handlers, embedded documents and unsafe styles', async () => {
    const main = await exported([
      '<script>alert(1)</script>',
      '<style>body { display: none }</style>',
      '<iframe src="https://example.com"></iframe>',
      '<img src="x.png" onerror="alert(1)">',
      '<a href="javascript:alert(1)">脚本链接</a> <a href="java&#x09;script:alert(1)">编码链接</a> <a href="data:text/html,x">数据</a>',
      '<span style="color: red; background: url(https://evil.example/t.png)">彩色</span> <span style="width: expression(alert(1))">表达式</span>',
      '<svg onload="alert(1)"><text>矢量</text></svg> <object data="x.swf">对象</object> <custom-tag>自定义</custom-tag>',
      '<form action="https://evil.example"><input name="q"><button>按钮</button></form>',
    ].join('\n\n'))
    expect(main).not.toMatch(/<script|<style|<iframe|<svg|<object|<form|<input|onerror|onload|javascript|expression|url\(|evil|alert/iu)
    expect(main).toContain('<span style="color:red">彩色</span>')
    expect(main).toContain('自定义')
    expect(main).toContain('按钮')
    expect(main).not.toContain('矢量')
    expect(main).not.toContain('对象')
    expect(main).toContain('脚本链接')
  })

  it('closes what a document leaves open without hiding the blocks after it', async () => {
    const main = await exported('甲 <span>未闭合\n\n<iframe src="x">\n\n乙段落\n\n<table><tr><td>格</td></tr></table>\n')
    expect(main).toMatch(/<p>甲 <span>未闭合<\/span><\/p>/u)
    expect(main).toContain('<p>乙段落</p>')
    expect(main).toMatch(/<table><tbody><tr><td>格<\/td><\/tr><\/tbody><\/table>/u)
  })

  it('reads highlights, keeps dollar amounts and single tildes as text, and labels footnotes in Chinese', async () => {
    const main = await exported('==高亮== 与 `==代码==`，价格 $5 到 $10，H~2~O 和 ~~删除~~，公式 $x^2$。注[^1]\n\n[^1]: 注释。\n')
    expect(main).toContain('<mark>高亮</mark>')
    expect(main).toContain('<code>==代码==</code>')
    expect(main).toContain('价格 $5 到 $10')
    expect(main).toContain('H~2~O')
    expect(main).toContain('<del>删除</del>')
    expect(main.match(/<math/gu)).toHaveLength(1)
    expect(main).toContain('脚注')
    expect(main).toContain('返回正文')
    expect(main).not.toContain('Footnotes')
  })
})
