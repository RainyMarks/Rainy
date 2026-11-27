// @vitest-environment happy-dom
/** Regression tests covering P0 security/crash fixes and P1 bug/leak/protocol fixes across viewer pages. */
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import * as XLSX from 'xlsx'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { effectiveAccent, normalizeAccent, normalizeColorScheme } from '../../src/shared/appearance.ts'
import type { IdeWorkspace, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { FootprintTurn } from '../../src/shared/map-protocol.ts'
import type { HostMethod, SessionId } from '../../src/shared/rpc.ts'
import { ByteSource } from '../../src/renderer/ide/viewers/byte-source.ts'
import { sanitizeDocxDom, sanitizeDocxStyles } from '../../src/renderer/ide/viewers/docx-sanitize.ts'
import { markdownImageTarget } from '../../src/renderer/ide/viewers/markdown-assets.ts'
import { embedImage, exportHtml, resolveExportImagePath } from '../../src/renderer/ide/viewers/markdown-export.ts'
import { preserveSource, rebaseEdit } from '../../src/renderer/ide/viewers/markdown-merge.ts'
import { markdownOutline } from '../../src/renderer/ide/viewers/markdown-outline.ts'
import { OfficeLockViewer, SheetViewer, WordViewer } from '../../src/renderer/ide/viewers/OfficeViewers.tsx'
import { PdfViewer } from '../../src/renderer/ide/viewers/PdfViewer.tsx'
import { remarkReferences } from '../../src/renderer/ide/viewers/rich-syntax.ts'
import { MAX_SHEET_COLUMNS, MAX_SHEET_ROWS, parseWorkbookRaw } from '../../src/renderer/ide/viewers/sheet-format.ts'
import { FootprintView } from '../../src/renderer/map/Footprint.tsx'
import { mapMessages } from '../../src/renderer/map/messages.ts'
import { resolveOfficePdfResponse } from '../../src/renderer/viewer/ViewerApp.tsx'
import { host } from './ide-host-mock.ts'

const docxMock = vi.hoisted(() => ({
  renderAsync: vi.fn<(...args: unknown[]) => Promise<void>>(),
}))

vi.mock('docx-preview', () => ({ renderAsync: docxMock.renderAsync }))
vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
vi.mock('../../src/renderer/ide/viewers/mermaid.ts', () => ({
  isMermaid: (language: string | undefined) => language === 'mermaid',
  renderMermaid: async (source: string) => `<svg data-test="diagram">${source.length}</svg>`,
  onThemeChange: () => () => undefined,
}))

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

const roots: Root[] = []
function mount(element: JSX.Element): HTMLElement {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  roots.push(root)
  act(() => { root.render(element) })
  return container
}

async function flush(ms = 25): Promise<void> {
  await act(async () => { await new Promise(resolve => setTimeout(resolve, ms)) })
}

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (!predicate() && Date.now() < deadline) await flush(15)
}

function binaryResponse(bytes: Uint8Array, headers?: HeadersInit): Response {
  const copy = new Uint8Array(bytes.byteLength)
  copy.set(bytes)
  return new Response(new Blob([copy]), { status: 200, headers })
}

beforeEach(() => {
  docxMock.renderAsync.mockReset()
  host.call.mockReset()
})

afterEach(() => {
  for (const root of roots.splice(0)) act(() => { root.unmount() })
  document.body.replaceChildren()
  delete (globalThis as { pdfjsLib?: unknown }).pdfjsLib
  vi.restoreAllMocks()
})

describe('P0: DOCX off-screen sanitization, CSS @import truncation, SVG/template tags, and auto-fit width', () => {
  it('sanitizes docx-preview off-screen before attaching to Shadow DOM and computes auto-fit width after unhiding', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(binaryResponse(new Uint8Array([0x50, 0x4b, 0x03, 0x04])))
    let connectedDuringRender = true
    docxMock.renderAsync.mockImplementation(async (_bytes, body, styles) => {
      const bodyEl = body as HTMLElement
      const stylesEl = styles as HTMLElement
      // Must be detached off-screen while docx-preview populates unsanitized DOM.
      connectedDuringRender = bodyEl.isConnected || stylesEl.isConnected
      const page = document.createElement('section')
      page.className = 'docx'
      Object.defineProperty(page, 'offsetWidth', { value: 1600, configurable: true })
      page.innerHTML = '<template><script>alert(1)</script></template><svg><animate attributeName="href" to="javascript:alert(1)" /><feImage href="https://evil.example/a.png" /><set attributeName="onbegin" to="alert(1)" /></svg><p>safe</p>'
      bodyEl.append(page)
      stylesEl.innerHTML = '<a href="javascript:alert(1)">bad-in-styles</a><style>@import "https://evil.example/a;b.css"; @import url("https://evil.example/x}y.css"); .ok { color: #333; }</style>'
    })

    const container = mount(<WordViewer url="/rainy/ide/raw?w=w&p=report.docx" size={16} location={{ name: 'report.docx', extension: 'docx' }} />)
    await flush(40)

    expect(connectedDuringRender).toBe(false)
    const shadow = [...container.querySelectorAll('div')].find(el => el.shadowRoot !== null)!.shadowRoot!
    expect(shadow.querySelector('template')).toBeNull()
    expect(shadow.querySelector('animate')).toBeNull()
    expect(shadow.querySelector('feimage')).toBeNull()
    expect(shadow.querySelector('set')).toBeNull()
    expect(shadow.querySelector('a')?.hasAttribute('href')).toBe(false)
    const cssText = [...shadow.querySelectorAll('style')].map(el => el.textContent ?? '').join('\n')
    expect(cssText).not.toMatch(/@import|evil\.example/iu)
    expect(cssText).toContain('color: #333')
    // Default stage width 800 minus 40 padding / 1600 page width = 0.475 -> 48% auto-fit zoom (not stuck at 100%).
    expect(container.textContent).toContain('48%')
  })

  it('strips @import rules containing quoted semicolons or braces without truncating', () => {
    const css = '@import "https://evil.example/a;b}c.css" screen;\n@import url(\'https://evil.example/d;e.css\');\n.keep { color: blue; }'
    const clean = sanitizeDocxStyles(css)
    expect(clean).not.toMatch(/@import|evil\.example/iu)
    expect(clean).toContain('.keep { color: blue; }')
  })

  it('removes template, animate, set and feImage elements from detached DOCX containers', () => {
    const styles = document.createElement('div')
    const body = document.createElement('div')
    styles.innerHTML = '<a href="javascript:alert(1)">x</a>'
    body.innerHTML = '<template><img src="https://evil.example/t.png"></template><svg><animate /><set /><feImage href="https://evil.example/x.png" /></svg>'
    sanitizeDocxDom(styles, body)
    expect(styles.querySelector('a')?.hasAttribute('href')).toBe(false)
    expect(body.querySelector('template')).toBeNull()
    expect(body.querySelector('animate, set, feImage, feimage')).toBeNull()
  })
})

describe('P0: Markdown HTML self-closing dangerous tag bypass', () => {
  it('strips self-closing script/style/iframe/svg/math tags and their trailing payloads', async () => {
    const html = await exportHtml({
      text: [
        '<script/>alert(1)</script>',
        '<style/>body { display: none }</style>',
        '<iframe/>https://evil.example</iframe>',
        '<svg/>vector-payload</svg>',
        '<math/>math-payload</math>',
        '可见正文',
      ].join('\n\n'),
      name: 'xss',
      image: async () => undefined,
    })
    const main = html.slice(html.indexOf('<main>'), html.indexOf('</main>'))
    expect(main).not.toMatch(/alert\(1\)|display:\s*none|evil\.example|vector-payload|math-payload|<script|<style|<iframe|<svg/iu)
    expect(main).toContain('可见正文')
  })
})

describe('P0: Spreadsheet !merges DoS cap and SheetViewer row virtualization', () => {
  it('clamps malicious million-row !merges to MAX_SHEET_ROWS/MAX_SHEET_COLUMNS without OOM', async () => {
    const wb = XLSX.utils.book_new()
    const ws = XLSX.utils.aoa_to_sheet([['A1', 'B1'], ['A2', 'B2']])
    ws['!merges'] = [
      { s: { r: 0, c: 0 }, e: { r: 1_000_000, c: 500 } },
      { s: { r: MAX_SHEET_ROWS + 10, c: 0 }, e: { r: MAX_SHEET_ROWS + 50, c: 2 } },
      { s: { r: 0, c: MAX_SHEET_COLUMNS + 5 }, e: { r: 5, c: MAX_SHEET_COLUMNS + 10 } },
    ]
    XLSX.utils.book_append_sheet(wb, ws, 'Malicious')
    const bytes = new Uint8Array(XLSX.write(wb, { type: 'array', bookType: 'xlsx' }) as ArrayBuffer)
    const [sheet] = await parseWorkbookRaw(XLSX, bytes, 'zh')
    expect(sheet).toBeDefined()
    expect(sheet!.spans.length).toBe(1)
    expect(sheet!.spans[0]![1].rows).toBeLessThanOrEqual(sheet!.rows.length)
    expect(sheet!.spans[0]![1].columns).toBeLessThanOrEqual(MAX_SHEET_COLUMNS)
    expect(sheet!.hidden.length).toBeLessThan(1000)
  })

  it('virtualizes large sheets so only a bounded window of rows mounts in DOM', async () => {
    const wb = XLSX.utils.book_new()
    const aoa = Array.from({ length: 400 }, (_unused, row) => [`R${row}`, `V${row}`])
    XLSX.utils.book_append_sheet(wb, XLSX.utils.aoa_to_sheet(aoa), 'Big')
    const bytes = new Uint8Array(XLSX.write(wb, { type: 'array', bookType: 'xlsx' }) as ArrayBuffer)
    const savedWorker = globalThis.Worker
    vi.stubGlobal('Worker', undefined)
    try {
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(binaryResponse(bytes))
      const container = mount(<SheetViewer url="/rainy/ide/raw?w=w&p=big.xlsx" size={bytes.byteLength} location={{ name: 'big.xlsx', extension: 'xlsx' }} />)
      await waitFor(() => container.querySelector('tbody tr[data-row]') !== null)
      const renderedRows = container.querySelectorAll('tbody tr[data-row]')
      expect(renderedRows.length).toBeGreaterThan(0)
      expect(renderedRows.length).toBeLessThan(120)
    } finally {
      vi.stubGlobal('Worker', savedWorker)
    }
  })
})

describe('P0: Markdown export image MIME guard, deduplication, locale and local link stripping', () => {
  it('rejects non-image payloads in embedImage and allows real image bytes', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch')
    fetchSpy.mockResolvedValueOnce(new Response('secret=123', { status: 200, headers: { 'content-type': 'text/plain' } }))
    expect(await embedImage('/rainy/ide/raw?w=w&p=.env')).toBeUndefined()

    fetchSpy.mockResolvedValueOnce(new Response('<html>bad</html>', { status: 200, headers: { 'content-type': 'text/html' } }))
    expect(await embedImage('/rainy/ide/raw?w=w&p=index.html')).toBeUndefined()

    const pngHead = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d])
    fetchSpy.mockResolvedValueOnce(binaryResponse(pngHead, { 'content-type': 'application/octet-stream' }))
    const embedded = await embedImage('/rainy/ide/raw?w=w&p=logo.bin')
    expect(embedded).toMatch(/^data:image\/png;base64,/u)
  })

  it('deduplicates repeated image URLs, respects English locale, and strips file:// and drive links', async () => {
    const image = vi.fn(async (target: string) => target === 'a.png' ? 'data:image/png;base64,AAAA' : undefined)
    const html = await exportHtml({
      text: '[TOC]\n\n# Intro\n\n![1](a.png) ![2](a.png) ![3](C:\\Windows\\win.ini)\n\n[local](file:///C:/secret.txt) [drive](C:\\Windows\\win.ini) Note[^1]\n\n[^1]: Footnote text.\n',
      name: 'doc',
      locale: 'en',
      image,
    })
    expect(image).toHaveBeenCalledWith('a.png')
    expect(image.mock.calls.filter(([t]) => t === 'a.png')).toHaveLength(1)
    expect(html).toContain('<html lang="en">')
    expect(html).toContain('aria-label="Contents"')
    expect(html).toContain('Footnotes')
    expect(html).toContain('Back to the text')
    expect(html).not.toMatch(/href="file:|href="C:\\|src="C:\\/iu)
  })

  it('resolves in-project Windows drive image paths and blocks outside-project paths', () => {
    expect(resolveExportImagePath('docs', 'C:\\work\\proj\\assets\\fig.png', 'C:\\work\\proj')).toBe('assets/fig.png')
    expect(resolveExportImagePath('docs', 'C:\\Windows\\win.ini', 'C:\\work\\proj')).toBeUndefined()
    expect(resolveExportImagePath('docs', 'C:\\work\\proj\\..\\secret.png', 'C:\\work\\proj')).toBeUndefined()
  })
})

describe('P1: appearance normalization, outline, assets, merge, references, ByteSource, PdfViewer, Footprint, OfficeLock', () => {
  it('normalizes legacy "neutral" and unknown schemes/accents so data-accent is never undefined', () => {
    expect(normalizeColorScheme('neutral')).toBe('graphite')
    expect(normalizeColorScheme('unknown')).toBe('graphite')
    expect(normalizeAccent('weird')).toBe('auto')
    expect(effectiveAccent('neutral', 'auto')).toBe('blue')
    expect(effectiveAccent('midnight', 'auto')).toBe('violet')
  })

  it('escapes #, ?, %, brackets and backslashes in markdownImageTarget', () => {
    expect(markdownImageTarget('assets', 'a#1?b%2[c].png')).toBe('assets/a%231%3Fb%252%5Bc%5D.png')
  })

  it('skips indented code blocks in markdownOutline', () => {
    const outline = markdownOutline('# Real Title\n\n    # Not a heading\n\t## Also code\n\n## Second\n')
    expect(outline.map(item => item.text)).toEqual(['Real Title', 'Second'])
  })

  it('preserves undefined imageReference as literal text instead of deleting it', () => {
    const tree = {
      type: 'root',
      children: [{
        type: 'paragraph',
        children: [{ type: 'imageReference', alt: 'fig', label: 'missing', identifier: 'missing', referenceType: 'full' as const }],
      }],
    }
    remarkReferences()(tree)
    expect(tree.children[0]!.children).toEqual([{ type: 'text', value: '![fig][missing]' }])
  })

  it('handles mixed CRLF/LF in rebaseEdit and keeps trailing newline in preserveSource when deleting last line', () => {
    expect(rebaseEdit('a\r\n\r\nb\r\n\r\nc\r\n', 'a1\n\nb\n\nc\n', 'a\r\n\r\nb\r\n\r\nc1\r\n')).toBe('a1\r\n\r\nb\r\n\r\nc1\r\n')
    expect(preserveSource('a\n*b*\n', 'a\n_b_', 'a')).toBe('a\n')
  })

  it('aborts in-flight ByteSource reads when AbortSignal fires', async () => {
    let aborted = false
    const fetcher: typeof fetch = (_input, init) => new Promise((_resolve, reject) => {
      init?.signal?.addEventListener('abort', () => {
        aborted = true
        const err = new Error('aborted')
        err.name = 'AbortError'
        reject(err)
      })
    })
    const source = new ByteSource('/raw', 128, 8, fetcher)
    const controller = new AbortController()
    const promise = source.read(0, 64, controller.signal)
    controller.abort()
    await expect(promise).rejects.toMatchObject({ name: 'AbortError' })
    expect(aborted).toBe(true)
  })

  it('handles invalid base64 in resolveOfficePdfResponse without throwing', async () => {
    const result = await resolveOfficePdfResponse({ pdf: '!!!not-base64!!!' })
    expect('pdf' in result).toBe(false)
    if (!('pdf' in result)) expect(result.code).toBe('failed')
  })

  it('opens a 0-page PDF cleanly without throwing on getPage(1)', async () => {
    const getPage = vi.fn()
    ;(globalThis as { pdfjsLib?: unknown }).pdfjsLib = {
      GlobalWorkerOptions: { workerSrc: '' },
      PasswordResponses: { NEED_PASSWORD: 1, INCORRECT_PASSWORD: 2 },
      getDocument: vi.fn(() => ({ promise: Promise.resolve({ numPages: 0, getPage, getDestination: async () => null, getPageIndex: async () => 0 }), destroy: async () => undefined, onPassword: null })),
      renderTextLayer: () => ({ promise: Promise.resolve(), cancel: () => undefined }),
      AnnotationLayer: { render: () => undefined },
    }
    const container = mount(<PdfViewer url="/rainy/ide/raw?w=w&p=empty.pdf" bytes={128} />)
    await flush(40)
    expect(getPage).not.toHaveBeenCalled()
    expect(container.querySelector('[data-page]')).toBeNull()
    expect(container.textContent).toContain('0 / 0')
  })

  it('filters deleted and outside-project paths from FootprintView changed-file buttons', async () => {
    const workspace: IdeWorkspace = { workspaceId: 'w1' as WorkspaceId, path: 'C:\\repo', title: 'repo' }
    const turns: FootprintTurn[] = [{
      id: 'turn-1',
      ts: 1000,
      text: 'update files',
      steps: [
        { action: 'edit', path: 'C:\\repo\\src\\ok.ts', label: 'src/ok.ts', ok: true },
        { action: 'delete', path: 'C:\\repo\\src\\gone.ts', label: 'src/gone.ts', ok: true },
        { action: 'create', path: 'C:\\outside\\leak.txt', label: 'C:\\outside\\leak.txt', ok: true },
      ],
    }]
    host.call.mockImplementation((method: HostMethod) => method === 'map.footprint' ? Promise.resolve({ turns }) : Promise.resolve([]))
    const opened: string[] = []
    const container = mount(
      <FootprintView workspace={workspace} sessionId={'s1' as SessionId} onOpenFile={(key) => { opened.push(key) }} t={mapMessages.t} />,
    )
    await waitFor(() => container.querySelector('[data-map-footprint] article') !== null)
    const changedSection = container.querySelector('article section:first-of-type')!
    const buttons = [...changedSection.querySelectorAll('button')].map(btn => btn.textContent)
    expect(buttons).toEqual(['src/ok.ts'])
  })

  it('displays load error in OfficeLockViewer when fetching lock file fails', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('', { status: 404 }))
    const container = mount(<OfficeLockViewer url="/rainy/ide/raw?w=w&p=~$doc.docx" size={64} document="doc.docx" />)
    await waitFor(() => container.querySelector('[role="alert"]') !== null)
    expect(container.querySelector('[role="alert"]')).not.toBeNull()
  })
})
