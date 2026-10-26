// @vitest-environment happy-dom
/** The Markdown preview of project files: allowlisted HTML, footnotes, links, highlights, dollar amounts, Mermaid diagrams
 *  and scroll sync with the source editor. */
import { act, createElement, type ReactElement } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { Markdown, MarkdownDocument, type MarkdownOptions } from '../../src/renderer/chat/Markdown.tsx'
import { MarkdownPreview } from '../../src/renderer/ide/viewers/MarkdownPreview.tsx'
import { onThemeChange } from '../../src/renderer/ide/viewers/mermaid.ts'
import { ScrollSync } from '../../src/renderer/ide/viewers/scroll-sync.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'

const mermaid = vi.hoisted(() => ({
  render: vi.fn(async (source: string) => {
    if (source.includes('坏')) throw new Error('Parse error on line 2')
    return `<svg data-test="diagram">${source.length}</svg>`
  }),
}))
vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
vi.mock('../../src/renderer/ide/viewers/mermaid.ts', async importOriginal => ({
  ...await importOriginal<typeof import('../../src/renderer/ide/viewers/mermaid.ts')>(),
  renderMermaid: mermaid.render,
}))

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true
Object.defineProperty(document, 'compatMode', { value: 'CSS1Compat', configurable: true })

const scrolledTo = vi.fn<(id: string) => void>()
Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { value(this: HTMLElement) { scrolledTo(this.id) }, configurable: true })

const unmounts: Array<() => void> = []
beforeEach(() => { scrolledTo.mockClear(); mermaid.render.mockClear() })
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

const BASE = { workspaceId: 'w' as WorkspaceId, directory: 'docs' }

async function wait(ms: number): Promise<void> {
  await act(async () => { await new Promise(resolve => setTimeout(resolve, ms)) })
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

  it('draws [TOC] as nested links to the headings, without the footnotes', async () => {
    const host = await document_('[TOC]\n\n# 第一章\n\n## 小节 *一*\n\n# 第一章\n\n正文[^1]\n\n\\[TOC]\n\n[^1]: 注。\n', { file: { ...FILE.file!, tocLabel: '目录' } })
    const toc = host.querySelector('nav.md-toc')!
    expect(toc.getAttribute('aria-label')).toBe('目录')
    expect([...toc.querySelectorAll(':scope > ul > li > a')].map(link => [link.textContent, link.getAttribute('href')]))
      .toEqual([['第一章', '#md-第一章'], ['第一章', '#md-第一章-1']])
    expect(toc.querySelector(':scope > ul > li > ul > li > a')?.textContent).toBe('小节 一')
    expect(host.querySelectorAll('nav.md-toc')).toHaveLength(1)
    expect(host.textContent).toContain('[TOC]')
    await act(async () => { toc.querySelectorAll<HTMLAnchorElement>('a')[2]!.click() })
    expect(scrolledTo.mock.calls.map(call => call[0])).toEqual(['md-第一章-1'])
  })
})

describe('MarkdownPreview', () => {
  it('opens project files through openLocal, resolved against the document', async () => {
    const openLocal = vi.fn()
    const host = await mount(createElement(MarkdownPreview, { text: '[附录](appendix.md#L12) [外面](../../x.md) [网站](https://example.com)\n', base: BASE, version: null, openLocal }))
    const [inside, outside, web] = [...host.querySelectorAll<HTMLAnchorElement>('a')]
    await act(async () => { inside!.click(); outside!.click() })
    expect(openLocal.mock.calls).toEqual([[{ path: 'docs/appendix.md', line: 12 }]])
    expect(web!.target).toBe('_blank')
    expect(web!.rel).toBe('noreferrer')
  })

  it('draws Mermaid fences, explains failures in Chinese and draws again for a new theme', async () => {
    const host = await mount(createElement(MarkdownPreview, { text: '```mermaid\ngraph TD\nA-->B\n```\n\n```mermaid\ngraph TD\n坏\n```\n', base: BASE, version: null, openLocal: vi.fn() }))
    await wait(20)
    expect(host.querySelector('svg[data-test="diagram"]')).not.toBeNull()
    expect(host.querySelector('[role="alert"]')?.textContent).toBe('无法绘制图表：Parse error on line 2')
    // The failed diagram still shows its source.
    expect(host.textContent).toContain('坏')
    expect(mermaid.render).toHaveBeenCalledTimes(2)
    await act(async () => { document.body.setAttribute('data-rainy-dark', '') })
    await wait(20)
    expect(mermaid.render).toHaveBeenCalledTimes(4)
  })

  it('follows the editor and reports only the scrolling the user does', async () => {
    // Blocks 20 px apart per source line, as laid out in a stage scrolled by `scrollTop`.
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      const stage = this.closest('[data-viewer]')?.children[1] as HTMLElement | undefined
      const top = this.dataset.line === undefined ? 0 : Number(this.dataset.line) * 20 - (stage?.scrollTop ?? 0)
      return { top, bottom: top + 20, left: 0, right: 100, width: 100, height: 20, x: 0, y: top, toJSON: () => ({}) }
    })
    const sync = new ScrollSync()
    // A line sent before the preview subscribed is applied when it does.
    sync.editorScrolled(3)
    const lines: number[] = []
    const host = await mount(createElement(MarkdownPreview, {
      text: '# 一\n\n段落二\n\n段落三\n\n段落四\n', base: BASE, version: null, openLocal: vi.fn(), sync, onScrollLine: (line: number) => { lines.push(line) },
    }))
    const stage = host.querySelector('[data-viewer]')!.children[1] as HTMLElement
    expect(stage.scrollTop).toBe(52)
    await act(async () => { stage.dispatchEvent(new Event('scroll')) })
    await wait(40)
    expect(lines).toEqual([])
    stage.scrollTop = 92
    await act(async () => { stage.dispatchEvent(new Event('scroll')); stage.dispatchEvent(new Event('scroll')) })
    await wait(40)
    expect(lines).toEqual([5])
    // The editor following the user does not pull the preview back while the user scrolls it.
    sync.editorScrolled(4.9)
    expect(stage.scrollTop).toBe(92)
    await wait(300)
    sync.editorScrolled(7)
    expect(stage.scrollTop).toBe(132)
  })
})

describe('onThemeChange', () => {
  it('reports changes of the mode, scheme or accent once, and not attribute writes of the same value', async () => {
    const listener = vi.fn()
    const stop = onThemeChange(listener)
    document.body.setAttribute('data-color-scheme', 'graphite')
    await Promise.resolve()
    document.body.setAttribute('data-color-scheme', 'ocean')
    document.body.setAttribute('data-rainy-dark', '')
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(listener).toHaveBeenCalledTimes(1)
    document.body.setAttribute('data-rainy-dark', '')
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(listener).toHaveBeenCalledTimes(1)
    stop()
    document.body.removeAttribute('data-rainy-dark')
    await new Promise(resolve => setTimeout(resolve, 0))
    expect(listener).toHaveBeenCalledTimes(1)
  })
})
