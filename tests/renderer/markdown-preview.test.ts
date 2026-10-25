// @vitest-environment happy-dom
/** The Markdown preview of project files: allowlisted HTML, footnotes, anchors, highlights and dollar amounts. */
import { act, createElement, type ReactElement } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Markdown, MarkdownDocument, type MarkdownOptions } from '../../src/renderer/chat/Markdown.tsx'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
Object.defineProperty(document, 'compatMode', { value: 'CSS1Compat', configurable: true })

const scrolledTo = vi.fn<(id: string) => void>()
Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { value(this: HTMLElement) { scrolledTo(this.id) }, configurable: true })

const unmounts: Array<() => void> = []
beforeEach(() => { scrolledTo.mockClear() })
afterEach(() => {
  for (const unmount of unmounts.splice(0)) act(() => { unmount() })
  document.body.removeAttribute('data-rainy-dark')
  vi.restoreAllMocks()
})

async function mount(element: ReactElement): Promise<HTMLElement> {
  const host = document.createElement('div')
  document.body.appendChild(host)
  const root = createRoot(host)
  await act(async () => { root.render(element) })
  unmounts.push(() => { root.unmount(); host.remove() })
  return host
}

const FILE: MarkdownOptions = {
  base: { workspaceId: 'w', directory: 'docs' }, sourceLines: true, codeActions: false,
  file: { footnoteLabel: '脚注', footnoteBackLabel: '返回正文' },
}

function document_(text: string, options: Partial<MarkdownOptions> = {}): Promise<HTMLElement> {
  return mount(createElement(MarkdownDocument, { text, options: { ...FILE, ...options } }))
}

describe('HTML in a file preview', () => {
  it('hides comments and shows Typora images at their zoom through the project', async () => {
    const host = await document_('<!-- 私密备注 -->\n\n正文 <!-- 行内备注 --> 继续\n\n<img src="assets/a.png" style="zoom:50%;" />\n')
    expect(host.textContent).not.toMatch(/备注|<!--/u)
    const image = host.querySelector('img')!
    expect(image.getAttribute('src')).toBe('/rainy/ide/raw?w=w&p=docs%2Fassets%2Fa.png')
    expect(image.style.getPropertyValue('zoom')).toBe('50%')
    // Line 5 of the source, for scroll sync.
    expect(image.dataset.line).toBe('5')
  })

  it('pairs tags across Markdown: inline sub, folded sections around paragraphs', async () => {
    const host = await document_('第一行<br>第二行 H<sub>2</sub>O\n\n<details>\n<summary>展开</summary>\n\n内容 **粗体**\n\n</details>\n\n之后\n')
    expect(host.querySelector('p br')).not.toBeNull()
    expect(host.querySelector('p sub')?.textContent).toBe('2')
    const details = host.querySelector('details')!
    expect(details.querySelector('summary')?.textContent).toBe('展开')
    expect(details.querySelector('p strong')?.textContent).toBe('粗体')
    expect(details.dataset.line).toBe('3')
    expect([...host.querySelectorAll('[data-markdown-root] > p')].at(-1)?.textContent).toBe('之后')
    expect(host.textContent).not.toContain('<')
  })

  it('runs nothing and fetches nothing from the web', async () => {
    const host = await document_([
      '<script>window.ran = 1</script>',
      '<img src="x.png" onerror="window.ran = 2"> <img src="https://example.com/t.png" width="100">',
      '<a href="javascript:alert(1)">链接</a> <span style="color: red; background: url(https://evil.example/t)">彩色</span>',
      '<iframe src="https://example.com"></iframe> <svg onload="alert(1)"><text>矢量</text></svg>',
    ].join('\n\n'))
    expect(host.innerHTML).not.toMatch(/<script|<iframe|<svg|onerror|onload|javascript|url\(|evil/iu)
    expect(host.textContent).not.toContain('矢量')
    expect([...host.querySelectorAll('img')].map(image => image.getAttribute('src'))).toEqual(['/rainy/ide/raw?w=w&p=docs%2Fx.png'])
    expect(host.textContent).toContain('远程图片未加载：example.com')
    expect(host.querySelector('span[style]')?.getAttribute('style')).toBe('color: red;')
  })

  it('shows images embedded as data URLs', async () => {
    const host = await document_('<img src="data:image/png;base64,AAAA" width="20"> ![点](data:image/png;base64,BBBB)\n\n[页](data:text/html,x)\n')
    expect([...host.querySelectorAll('img')].map(image => image.getAttribute('src'))).toEqual(['data:image/png;base64,AAAA', 'data:image/png;base64,BBBB'])
    expect(host.querySelector('img')?.getAttribute('width')).toBe('20')
    expect(host.querySelector('a')).toBeNull()
  })

  it('leaves assistant replies as they were: HTML shows as text', async () => {
    const host = await mount(createElement(Markdown, { text: '一 <b>二</b>\n' }))
    expect(host.textContent).toContain('<b>二</b>')
  })
})

describe('Markdown of a file preview', () => {
  it('reads highlights, keeps dollar amounts and single tildes as text', async () => {
    const host = await document_('==高亮== 与 `==代码==`，a == b，价格 $5 到 $10，H~2~O 与 ~~删除~~，公式 $x^2$\n')
    expect(host.querySelector('mark')?.textContent).toBe('高亮')
    expect(host.querySelectorAll('mark')).toHaveLength(1)
    expect(host.querySelector('code')?.textContent).toBe('==代码==')
    expect(host.textContent).toContain('价格 $5 到 $10')
    expect(host.textContent).toContain('H~2~O')
    expect(host.querySelector('del')?.textContent).toBe('删除')
    expect(host.querySelectorAll('.math-inline, .katex')).toHaveLength(1)
  })

  it('labels footnotes in Chinese and scrolls between a reference and its note', async () => {
    const host = await document_('正文[^1]。\n\n[^1]: 注释。\n')
    const section = host.querySelector('section[data-footnotes]')!
    expect(section.querySelector('h2')?.textContent).toBe('脚注')
    expect(host.textContent).not.toContain('Footnotes')
    const back = section.querySelector<HTMLAnchorElement>('a[data-footnote-backref]')!
    expect(back.getAttribute('aria-label')).toBe('返回正文')
    await act(async () => { host.querySelector<HTMLAnchorElement>('a[data-footnote-ref]')!.click() })
    await act(async () => { back.click() })
    expect(scrolledTo.mock.calls.map(call => call[0])).toEqual(['user-content-fn-1', 'user-content-fnref-1'])
  })

  it('scrolls to headings and HTML anchors', async () => {
    const host = await document_('# 第一章 概述\n\n<a id="附录A"></a>附录\n\n[到标题](#第一章-概述) [到附录](#附录A)\n')
    const links = [...host.querySelectorAll<HTMLAnchorElement>('a[href^="#"]')]
    await act(async () => { for (const link of links) link.click() })
    expect(scrolledTo.mock.calls.map(call => call[0])).toEqual(['md-第一章-概述', 'md-附录A'])
  })
})
