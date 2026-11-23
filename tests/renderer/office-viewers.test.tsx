// @vitest-environment happy-dom
/** Component and security tests for Office viewers: docx style sanitization, cancelled blob URL cleanup,
 *  zero-Base64 office-pdf streaming, and spreadsheet parsing with/without Web Worker. */
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import * as XLSX from 'xlsx'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { OfficePdfSource } from '../../src/shared/office-protocol.ts'
import { sanitizeDocxStyles } from '../../src/renderer/ide/viewers/docx-sanitize.ts'
import { OfficeViewer, SheetViewer, WordViewer } from '../../src/renderer/ide/viewers/OfficeViewers.tsx'
import { resolveOfficePdfResponse } from '../../src/renderer/viewer/ViewerApp.tsx'

const docxMock = vi.hoisted(() => ({
  renderAsync: vi.fn<(...args: unknown[]) => Promise<void>>(),
}))

vi.mock('docx-preview', () => ({ renderAsync: docxMock.renderAsync }))
vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

const roots: Root[] = []
function mount(element: JSX.Element): HTMLElement {
  const host = document.createElement('div')
  document.body.append(host)
  const root = createRoot(host)
  roots.push(root)
  act(() => { root.render(element) })
  return host
}

async function flush(ms = 20): Promise<void> {
  await act(async () => { await new Promise(resolve => setTimeout(resolve, ms)) })
}

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const deadline = Date.now() + timeoutMs
  while (!predicate() && Date.now() < deadline) {
    await flush(15)
  }
}

beforeEach(() => {
  docxMock.renderAsync.mockReset()
  delete window.__RAINY_OFFICE__
})

afterEach(() => {
  for (const root of roots.splice(0)) act(() => { root.unmount() })
  document.body.replaceChildren()
  delete window.__RAINY_OFFICE__
  delete (globalThis as { pdfjsLib?: unknown }).pdfjsLib
  vi.restoreAllMocks()
})

describe('sanitizeDocxStyles', () => {
  it('strips @import, expression(), obfuscated escapes, comments and external URLs while keeping blob/data assets', () => {
    const dirty = [
      '@import url("https://evil.example/a.css");',
      '@im/**/port "https://evil.example/b.css";',
      '\\69mport url(http://evil.example/c.css);',
      '.a { width: ex/**/pression(alert(1)); height: expr\\65ssion(nested(2)); behavior: url(evil.htc); -moz-binding: url(x.xml); }',
      '.b { background: url(https://evil.example/bg.png); mask: u\\72l("http://evil.example/m.png"); border-image: url(//evil.example/i.png); }',
      '.c { background-image: image-set("https://evil.example/a.png" 1x); content: -webkit-image-set("https://evil.example/b.png" 2x); list-style-image: url("https://evil.example/a)b.png"); }',
      '.ok { color: #222; background: url("blob:http://127.0.0.1:9/font-1"); list-style-image: url(data:image/png;base64,AAAA); content: "\\2022"; }',
    ].join('\n')
    const clean = sanitizeDocxStyles(dirty)
    expect(clean).not.toMatch(/@import|evil\.example|expression\s*\(|image-set\s*\(|behavior\s*:|-moz-binding|alert/iu)
    expect(clean).toContain('color: #222')
    expect(clean).toContain('url("blob:http://127.0.0.1:9/font-1")')
    expect(clean).toContain('url(data:image/png;base64,AAAA)')
    expect(clean).toContain('\\2022')
  })
})

function binaryResponse(bytes: Uint8Array, headers?: HeadersInit): Response {
  const copy = new Uint8Array(bytes.byteLength)
  copy.set(bytes)
  return new Response(new Blob([copy]), { status: 200, headers })
}

describe('WordViewer security and blob URL lifecycle', () => {
  it('sanitizes docx-preview styles/DOM and revokes blob URLs on unmount', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(binaryResponse(new Uint8Array([0x50, 0x4b, 0x03, 0x04])))
    const revoked: string[] = []
    vi.spyOn(URL, 'revokeObjectURL').mockImplementation((url: string) => { revoked.push(url) })

    docxMock.renderAsync.mockImplementation(async (_bytes, body, styles) => {
      const styleNode = document.createElement('style')
      styleNode.textContent = '@import "https://evil.example/x.css"; @font-face { src: url("blob:http://127.0.0.1/font-1"); } .p { background: url(https://evil.example/t.png); width: expression(alert(1)); }'
      ;(styles as HTMLElement).append(styleNode)
      ;(body as HTMLElement).innerHTML = '<section class="docx"><img src="blob:http://127.0.0.1/img-1" srcset="https://evil.example/2x.png 2x"><img src="https://evil.example/ext.png"><source src="https://evil.example/v.mp4" srcset="https://evil.example/s.png 1x"><svg><image href="https://evil.example/svg.png" /><image href="blob:http://127.0.0.1/svg-ok" /><use href="https://evil.example/s.svg#x" /><use href="#local-shape" /></svg><form action="https://evil.example/post"><button formaction="https://evil.example/btn">go</button></form><a href="javascript:alert(1)">bad</a><a href="https://example.com" ping="https://evil.example/ping">ok</a><script>window.__xss=1</script><p onclick="alert(1)" style="background:url(https://evil.example/s.png)">hi</p></section>'
    })

    const container = mount(<WordViewer url="/rainy/ide/raw?w=w&p=a.docx" size={16} location={{ name: 'a.docx', extension: 'docx' }} />)
    await flush()

    const shadow = [...container.querySelectorAll('div')].find(el => el.shadowRoot !== null)!.shadowRoot!
    const styleTexts = [...shadow.querySelectorAll('style')].map(s => s.textContent ?? '').join('\n')
    expect(styleTexts).not.toMatch(/@import|evil\.example|expression\(/iu)
    expect(styleTexts).toContain('blob:http://127.0.0.1/font-1')
    expect(shadow.querySelector('script')).toBeNull()
    expect(shadow.querySelector('p')?.getAttribute('onclick')).toBeNull()
    expect(shadow.querySelector('p')?.getAttribute('style')).not.toContain('evil.example')
    const links = [...shadow.querySelectorAll('a')]
    expect(links[0]!.hasAttribute('href')).toBe(false)
    expect(links[1]!.getAttribute('rel')).toBe('noopener noreferrer')
    expect(links[1]!.hasAttribute('ping')).toBe(false)
    const imgs = [...shadow.querySelectorAll('img')]
    expect(imgs[0]!.getAttribute('src')).toBe('blob:http://127.0.0.1/img-1')
    expect(imgs[0]!.hasAttribute('srcset')).toBe(false)
    expect(imgs[1]!.hasAttribute('src')).toBe(false)
    const source = shadow.querySelector('source')!
    expect(source.hasAttribute('src')).toBe(false)
    expect(source.hasAttribute('srcset')).toBe(false)
    const svgImages = [...shadow.querySelectorAll('image')]
    expect(svgImages[0]!.hasAttribute('href')).toBe(false)
    expect(svgImages[1]!.getAttribute('href')).toBe('blob:http://127.0.0.1/svg-ok')
    const svgUses = [...shadow.querySelectorAll('use')]
    expect(svgUses[0]!.hasAttribute('href')).toBe(false)
    expect(svgUses[1]!.getAttribute('href')).toBe('#local-shape')
    expect(shadow.querySelector('form')?.hasAttribute('action')).toBe(false)
    expect(shadow.querySelector('button')?.hasAttribute('formaction')).toBe(false)

    // Unmounting must revoke every blob URL from both styles and body.
    act(() => { roots.pop()!.unmount() })
    expect(revoked.sort()).toEqual(['blob:http://127.0.0.1/font-1', 'blob:http://127.0.0.1/img-1', 'blob:http://127.0.0.1/svg-ok'])
  })

  it('revokes late blob URLs created by docx-preview after unmount/cancellation', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(binaryResponse(new Uint8Array([0x50, 0x4b, 0x03, 0x04])))
    const revoked: string[] = []
    vi.spyOn(URL, 'revokeObjectURL').mockImplementation((url: string) => { revoked.push(url) })

    let finishRender: (() => void) | undefined
    docxMock.renderAsync.mockImplementation(async (_bytes, body, styles) => {
      await new Promise<void>((resolve) => { finishRender = resolve })
      const styleNode = document.createElement('style')
      styleNode.textContent = '@font-face { src: url("blob:http://127.0.0.1/late-font"); }'
      ;(styles as HTMLElement).append(styleNode)
      ;(body as HTMLElement).innerHTML = '<section class="docx"><img src="blob:http://127.0.0.1/late-img"></section>'
    })

    mount(<WordViewer url="/rainy/ide/raw?w=w&p=slow.docx" size={16} location={{ name: 'slow.docx', extension: 'docx' }} />)
    await flush()
    expect(finishRender).toBeDefined()

    // Unmount before renderAsync finishes.
    act(() => { roots.pop()!.unmount() })
    expect(revoked).toEqual([])

    // Now renderAsync resolves late and populates blob URLs; cancellation handler must revoke them.
    finishRender!()
    await flush()
    expect(revoked.sort()).toEqual(['blob:http://127.0.0.1/late-font', 'blob:http://127.0.0.1/late-img'])
  })
})

describe('zero-Base64 office-pdf consumer and OfficeViewer', () => {
  it('fetches binary PDF bytes from the one-shot route without Base64 and rejects off-origin URLs', async () => {
    const pdfBytes = new Uint8Array([0x25, 0x50, 0x44, 0x46, 0x2d, 0x31, 0x2e, 0x37])
    const fetcher = vi.fn<typeof fetch>(async () => binaryResponse(pdfBytes, { 'content-type': 'application/pdf' }))
    const ok = await resolveOfficePdfResponse({ url: '/rainy/ide/office-pdf?token=one-shot-123', application: 'PowerPoint' }, fetcher)
    expect(fetcher).toHaveBeenCalledOnce()
    expect(fetcher).toHaveBeenCalledWith('/rainy/ide/office-pdf?token=one-shot-123', { credentials: 'include', cache: 'no-store' })
    expect('pdf' in ok && ok.application === 'PowerPoint').toBe(true)
    if ('pdf' in ok) expect([...ok.pdf]).toEqual([...pdfBytes])

    const rejected = await resolveOfficePdfResponse({ url: 'https://evil.example/rainy/ide/office-pdf?token=x' }, fetcher)
    expect('pdf' in rejected).toBe(false)
    expect(fetcher).toHaveBeenCalledOnce()
  })

  it('passes workspace coordinates directly to __RAINY_OFFICE__.toPdf without pre-fetching raw bytes', async () => {
    const fetchSpy = vi.spyOn(globalThis, 'fetch')
    const toPdf = vi.fn(async (source: OfficePdfSource | Uint8Array) => {
      expect(source instanceof Uint8Array).toBe(false)
      return { code: 'missing' as const, error: 'no office', missing: true }
    })
    window.__RAINY_OFFICE__ = { toPdf: toPdf as NonNullable<typeof window.__RAINY_OFFICE__>['toPdf'] }

    mount(<OfficeViewer url="/rainy/ide/raw?w=w1&p=deck.ppt" size={1024}
      location={{ name: 'deck.ppt', extension: 'ppt', workspaceId: 'w1' as WorkspaceId, path: 'deck.ppt', version: 'v1' }} />)
    await flush()

    expect(toPdf).toHaveBeenCalledOnce()
    expect(toPdf).toHaveBeenCalledWith({ workspaceId: 'w1', rootId: undefined, path: 'deck.ppt', version: 'v1', extension: 'ppt' })
    // Legacy .ppt has no SlidesOutline fallback, so the raw 200 MB-capable byte route was never fetched into renderer memory.
    expect(fetchSpy).not.toHaveBeenCalled()
  })
})

describe('SheetViewer (Worker and chunked fallback)', () => {
  function sampleWorkbookBytes(): Uint8Array {
    const wb = XLSX.utils.book_new()
    const ws1 = XLSX.utils.aoa_to_sheet([
      ['标题', '', '数值'],
      ['甲', '乙', 42],
    ])
    ws1['!merges'] = [{ s: { r: 0, c: 0 }, e: { r: 0, c: 1 } }]
    const ws2 = XLSX.utils.aoa_to_sheet([['第二页', 'OK']])
    XLSX.utils.book_append_sheet(wb, ws1, '总览')
    XLSX.utils.book_append_sheet(wb, ws2, '明细')
    const arr = XLSX.write(wb, { type: 'array', bookType: 'xlsx' }) as ArrayBuffer
    return new Uint8Array(arr)
  }

  it('parses spreadsheets cleanly in environments without Web Worker support and renders merged cells and tabs', async () => {
    const savedWorker = globalThis.Worker
    vi.stubGlobal('Worker', undefined)
    try {
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(binaryResponse(sampleWorkbookBytes()))
      const container = mount(<SheetViewer url="/rainy/ide/raw?w=w&p=data.xlsx" size={2048} location={{ name: 'data.xlsx', extension: 'xlsx' }} />)
      await waitFor(() => container.querySelector('table') !== null)

      const merged = container.querySelector('td[colspan="2"]')
      expect(merged?.textContent).toBe('标题')
      expect(container.textContent).toContain('42')
      const tabs = [...container.querySelectorAll<HTMLButtonElement>('[role="tab"]')]
      expect(tabs.map(tab => tab.textContent)).toEqual(['总览', '明细'])
      act(() => { tabs[1]!.click() })
      expect(container.textContent).toContain('第二页')
    } finally {
      vi.stubGlobal('Worker', savedWorker)
    }
  })

  it('uses a transient Web Worker when available and terminates it after completion', async () => {
    const terminated = vi.fn()
    const posted: unknown[] = []
    class FakeWorker {
      onmessage: ((event: MessageEvent) => void) | null = null
      onerror: (() => void) | null = null
      onmessageerror: (() => void) | null = null
      postMessage(data: { id: number }): void {
        posted.push(data)
        queueMicrotask(() => {
          this.onmessage?.({
            data: {
              id: data.id,
              sheets: [{ name: 'WorkerSheet', rows: [['来自Worker', '99']], totalRows: 1, totalColumns: 2, spans: [], hidden: [] }],
            },
          } as MessageEvent)
        })
      }
      terminate(): void { terminated() }
    }
    const savedWorker = globalThis.Worker
    vi.stubGlobal('Worker', FakeWorker)
    try {
      vi.spyOn(globalThis, 'fetch').mockResolvedValue(binaryResponse(new Uint8Array([0x50, 0x4b, 0x03, 0x04])))
      const container = mount(<SheetViewer url="/rainy/ide/raw?w=w&p=w.xlsx" size={128} location={{ name: 'w.xlsx', extension: 'xlsx' }} />)
      await waitFor(() => container.querySelector('table') !== null)
      expect(posted).toHaveLength(1)
      expect(terminated).toHaveBeenCalledOnce()
      expect(container.textContent).toContain('来自Worker')
    } finally {
      vi.stubGlobal('Worker', savedWorker)
    }
  })
})
