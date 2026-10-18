// @vitest-environment happy-dom
/** Markdown of replies and previews: relative targets, guarded images, heading anchors and source lines. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { MarkdownDocument, resolveDocumentPath, slugify, type MarkdownOptions } from '../../src/renderer/chat/Markdown.tsx'
import { all, cleanup, click, render, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

afterEach(async () => { await cleanup() })

describe('resolveDocumentPath', () => {
  it('resolves against the document directory and the project root, and refuses to leave the project', () => {
    expect(resolveDocumentPath('docs/guide', 'img/a.png')).toBe('docs/guide/img/a.png')
    expect(resolveDocumentPath('docs/guide', '../shot%20one.png?raw=1#x')).toBe('docs/shot one.png')
    expect(resolveDocumentPath('docs', '/assets/logo.png')).toBe('assets/logo.png')
    expect(resolveDocumentPath('docs', '../../etc/passwd')).toBeUndefined()
    expect(resolveDocumentPath('', 'C:\\ctf\\a.png')).toBe('C:/ctf/a.png')
  })

  it('slugs headings like GitHub, keeping letters of every script', () => {
    expect([slugify('Hello, World!'), slugify('题目 1：签到'), slugify('  A  b ')]).toEqual(['hello-world', '题目-1签到', 'a--b'])
  })
})

describe('MarkdownDocument', () => {
  const base: MarkdownOptions = { base: { workspaceId: 'w1', directory: 'notes' }, sourceLines: true, codeActions: false, revision: 'r1' }

  it('shows project images through the byte route and never fetches web images', async () => {
    const view = await render(<MarkdownDocument options={base} text={'![plot](plot.png)\n\n![badge](https://example.com/badge.svg)\n\n![data](data:image/png;base64,AAAA)'} />)
    const images = all<HTMLImageElement>('img', view.container)
    expect(images.map(image => image.getAttribute('src'))).toEqual(['/rainy/ide/raw?w=w1&p=notes%2Fplot.png&v=r1'])
    expect(view.container.textContent).toContain('example.com')
    expect(all('a[target="_blank"]', view.container).map(link => link.getAttribute('href'))).toEqual(['https://example.com/badge.svg'])
  })

  it('opens file:line, drive and file: links in the editor and drops script links', async () => {
    const open = vi.fn()
    const view = await render(<MarkdownDocument options={{ openLocal: open }} text={'[a](main.ts:12) [b](C:/work/x.ts) [c](file:///C:/work/y.ts#L3) [d](javascript:alert(1))'} />)
    const links = all('a', view.container)
    expect(links.map(link => link.getAttribute('target'))).toEqual([null, null, null])
    for (const link of links) await click(link)
    expect(open.mock.calls.map(call => call[0])).toEqual([{ path: 'main.ts', line: 12 }, { path: 'C:/work/x.ts' }, { path: 'C:/work/y.ts', line: 3 }])
    expect(view.container.textContent).toContain('d')
  })

  it('in a reply without a project, a relative image becomes a link that opens the file', async () => {
    const open = vi.fn()
    const view = await render(<MarkdownDocument options={{ openLocal: open }} text="![shot](out/shot.png)" />)
    expect(all('img', view.container)).toHaveLength(0)
    await click(all('a', view.container)[0]!)
    expect(open).toHaveBeenCalledWith({ path: 'out/shot.png' })
  })

  it('prefixes heading ids, numbers repeats, marks top-level blocks with their line and follows local links', async () => {
    const open = vi.fn()
    const view = await render(<MarkdownDocument options={{ ...base, openLocal: open }}
      text={'# Intro\n\ntext [solve](../solve.py#L12)\n\n## Intro\n\n- item'} />)
    expect(all('h1, h2', view.container).map(heading => heading.id)).toEqual(['md-intro', 'md-intro-1'])
    expect(all('[data-line]', view.container).map(block => block.getAttribute('data-line'))).toEqual(['1', '3', '5', '7'])
    await click(all('a', view.container)[0]!)
    expect(open).toHaveBeenCalledWith({ path: 'solve.py', line: 12 })
  })
})

describe('loading on demand', () => {
  it('highlights code once its grammar loads, and renders math once KaTeX loads', async () => {
    const view = await render(<MarkdownDocument options={{ codeActions: false }} text={'```python\ndef solve():\n    return 1\n```\n\nArea $x^2$.'} />)
    await waitFor(() => { expect(view.container.querySelector('.hljs-keyword')?.textContent).toBe('def') })
    await waitFor(() => { expect(view.container.querySelector('.katex')).not.toBeNull() })
  })
})
