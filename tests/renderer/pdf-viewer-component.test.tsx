// @vitest-environment happy-dom
/** Component and LRU cache tests for PdfViewer: bounded find text cache eviction and Ctrl+F streaming search. */
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { PdfTextCache } from '../../src/renderer/ide/viewers/pdf-find.ts'
import { layOutText } from '../../src/renderer/ide/viewers/pdf-text.ts'
import { PdfViewer } from '../../src/renderer/ide/viewers/PdfViewer.tsx'

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

async function flush(ms = 30): Promise<void> {
  await act(async () => { await new Promise(resolve => setTimeout(resolve, ms)) })
}

afterEach(() => {
  for (const root of roots.splice(0)) act(() => { root.unmount() })
  document.body.replaceChildren()
  delete (globalThis as { pdfjsLib?: unknown }).pdfjsLib
  vi.restoreAllMocks()
})

describe('PdfTextCache bounded LRU', () => {
  it('evicts the least-recently-used page when maxPages is reached and refreshes recency on get()', () => {
    const cache = new PdfTextCache(3, 10_000)
    cache.set(1, layOutText([{ str: 'page-1' }]))
    cache.set(2, layOutText([{ str: 'page-2' }]))
    cache.set(3, layOutText([{ str: 'page-3' }]))
    // Touch page 1 so page 2 becomes the oldest.
    expect(cache.get(1)?.text).toBe('page-1')
    cache.set(4, layOutText([{ str: 'page-4' }]))
    expect(cache.size).toBe(3)
    expect(cache.get(2)).toBeUndefined()
    expect(cache.get(1)?.text).toBe('page-1')
    expect(cache.get(3)?.text).toBe('page-3')
    expect(cache.get(4)?.text).toBe('page-4')
  })

  it('enforces maxChars budget and skips single oversized pages without wiping cached pages', () => {
    const cache = new PdfTextCache(10, 20)
    cache.set(1, layOutText([{ str: '1234567890' }])) // 10 chars
    cache.set(2, layOutText([{ str: 'abcdefghij' }])) // 10 chars (total 20)
    expect(cache.size).toBe(2)
    expect(cache.chars).toBe(20)
    cache.set(3, layOutText([{ str: 'ZZZZZ' }])) // 5 chars -> evicts page 1
    expect(cache.get(1)).toBeUndefined()
    expect(cache.get(2)?.text).toBe('abcdefghij')
    expect(cache.chars).toBe(15)
    // Oversized single page (> 20 chars) is ignored so existing entries stay warm.
    cache.set(4, layOutText([{ str: 'x'.repeat(25) }]))
    expect(cache.get(4)).toBeUndefined()
    expect(cache.size).toBe(2)
  })
})

describe('PdfViewer component', () => {
  it('opens a mocked PDF document, runs Ctrl+F search across pages, and cleans up off-screen scanned pages', async () => {
    const cleanups: number[] = []
    const pageTexts = ['第一章 概论 Rainy', '第二章 架构', '第三章 rainy 性能']
    const fakeDocument = {
      numPages: pageTexts.length,
      getPage: vi.fn(async (pageNumber: number) => ({
        getViewport: ({ scale }: { scale: number }) => ({
          width: 600 * scale,
          height: 800 * scale,
          clone: () => ({ width: 600 * scale, height: 800 * scale, clone: () => ({}), convertToViewportPoint: () => [0, 0] }),
          convertToViewportPoint: () => [0, 0],
        }),
        render: () => ({ promise: Promise.resolve(), cancel: () => undefined }),
        getTextContent: async () => ({ items: [{ str: pageTexts[pageNumber - 1]! }], styles: {} }),
        getAnnotations: async () => [],
        cleanup: () => { cleanups.push(pageNumber); return true },
      })),
      getDestination: async () => null,
      getPageIndex: async () => 0,
    }
    ;(globalThis as { pdfjsLib?: unknown }).pdfjsLib = {
      GlobalWorkerOptions: { workerSrc: '' },
      PasswordResponses: { NEED_PASSWORD: 1, INCORRECT_PASSWORD: 2 },
      getDocument: vi.fn(() => ({ promise: Promise.resolve(fakeDocument), destroy: async () => undefined, onPassword: null })),
      renderTextLayer: () => ({ promise: Promise.resolve(), cancel: () => undefined }),
      AnnotationLayer: { render: () => undefined },
    }

    const container = mount(<PdfViewer url="/rainy/ide/raw?w=w&p=doc.pdf" bytes={4096} />)
    await flush(40)
    expect(container.querySelectorAll('[data-page]')).toHaveLength(3)

    // Open Ctrl+F find bar and type query.
    act(() => { window.dispatchEvent(new KeyboardEvent('keydown', { key: 'f', ctrlKey: true, bubbles: true })) })
    const input = container.querySelector<HTMLInputElement>('[data-pdf-find] input')
    expect(input).not.toBeNull()
    act(() => {
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!
      setter.call(input!, 'rainy')
      input!.dispatchEvent(new Event('input', { bubbles: true }))
    })
    await flush(220)
    expect(container.querySelector('.rainy-find-count')?.textContent).toBe('1 / 2')
    // Off-screen pages scanned during search should have called cleanup() to release pdf.js resources.
    expect(cleanups.sort()).toEqual([1, 2, 3])

    // Toggle case sensitivity ("Aa") -> only lowercase "rainy" on page 3 matches.
    const toggle = container.querySelector<HTMLButtonElement>('.rainy-find-toggle')!
    act(() => { toggle.click() })
    await flush(220)
    expect(container.querySelector('.rainy-find-count')?.textContent).toBe('1 / 1')
  })
})
