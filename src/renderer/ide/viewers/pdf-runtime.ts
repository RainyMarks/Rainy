/** pdf.js 2.16 runtime types, script loader, link service and selection helper for PdfViewer. */
import { PdfJsUnavailableError } from './pdf-errors.ts'
import type { PdfTextItem, TextLayerRecord } from './pdf-text.ts'
import css from './Viewers.module.css'

/** The parts of the pdf.js 2.16 API this viewer uses. */
export interface PdfViewport {
  readonly width: number
  readonly height: number
  clone(options: { dontFlip: boolean }): PdfViewport
  /** PDF user space to CSS pixels of the viewport, y going down. */
  convertToViewportPoint(x: number, y: number): number[]
}
export interface PdfTask { readonly promise: Promise<unknown>; cancel(): void }
export interface PdfTextContent { readonly items: readonly PdfTextItem[]; readonly styles: Readonly<Record<string, unknown>> }
/** An annotation as `getAnnotations()` gives it; the annotation layer reads the other fields. */
export interface PdfAnnotation { readonly annotationType: number }
export interface PdfPage {
  getViewport(options: { scale: number }): PdfViewport
  render(options: { canvasContext: CanvasRenderingContext2D; viewport: PdfViewport }): PdfTask
  getTextContent(): Promise<PdfTextContent>
  getAnnotations(): Promise<PdfAnnotation[]>
  /** Frees what the page parsed for rendering once no render of it runs; the next render parses it again. */
  cleanup(): boolean
}
export interface PdfDocument {
  readonly numPages: number
  getPage(page: number): Promise<PdfPage>
  getDestination(name: string): Promise<unknown[] | null>
  getPageIndex(ref: unknown): Promise<number>
}
export interface PdfLoadingTask {
  readonly promise: Promise<PdfDocument>
  /** Stops loading, or closes the document once it is open. */
  destroy(): Promise<void>
  /** Called instead of failing when the file needs a password, and again with `INCORRECT_PASSWORD` after a wrong one. */
  onPassword: ((update: (password: string) => void, reason: number) => void) | null
}
/** What the annotation layer calls back for links: the part of pdf.js's `PDFLinkService` it uses. */
export interface PdfLinkService {
  addLinkAttributes(link: HTMLAnchorElement, url: string, newWindow?: boolean): void
  getDestinationHash(destination: unknown): string
  getAnchorUrl(hash: string): string
  goToDestination(destination: unknown): Promise<void>
  executeNamedAction(action: string): void
}
export interface PdfJs {
  GlobalWorkerOptions: { workerSrc: string }
  getDocument(options: Record<string, unknown>): PdfLoadingTask
  readonly PasswordResponses: { readonly NEED_PASSWORD: number; readonly INCORRECT_PASSWORD: number }
  renderTextLayer(options: { textContent: PdfTextContent; container: HTMLElement; viewport: PdfViewport; textDivs: HTMLElement[] }): PdfTask
  readonly AnnotationLayer: {
    render(options: {
      viewport: PdfViewport; div: HTMLElement; annotations: readonly PdfAnnotation[]; page: PdfPage; linkService: PdfLinkService; renderForms: boolean
    }): void
  }
}

/** What the pages of one open document share. */
export interface PdfContext {
  readonly library: PdfJs
  readonly document: PdfDocument
  readonly links: PdfLinkService
  /** Text layers of the rendered pages by page number, for copying a selection. */
  readonly layers: Map<number, TextLayerRecord>
}

export const PDFJS_ROOT = '/rainy/pdfjs/'

let loading: Promise<PdfJs> | undefined

/** @returns pdf.js, loaded once from the resources the Host serves. */
export function loadPdfJs(): Promise<PdfJs> {
  loading ??= new Promise<PdfJs>((resolve, reject) => {
    const global = globalThis as { pdfjsLib?: PdfJs }
    if (global.pdfjsLib !== undefined) { resolve(global.pdfjsLib); return }
    const script = document.createElement('script')
    script.src = `${PDFJS_ROOT}pdf.min.js`
    script.onload = () => {
      if (global.pdfjsLib === undefined) {
        script.remove()
        reject(new PdfJsUnavailableError('pdf.js did not load'))
        return
      }
      global.pdfjsLib.GlobalWorkerOptions.workerSrc = `${PDFJS_ROOT}pdf.worker.min.js`
      resolve(global.pdfjsLib)
    }
    script.onerror = () => {
      script.remove()
      reject(new PdfJsUnavailableError('pdf.js is unavailable'))
    }
    document.head.append(script)
  }).catch((error: unknown) => { loading = undefined; throw error })
  return loading
}

/**
 * Links of the annotation layer. Web addresses open through `window.open`, which the desktop hands to the system browser
 * (`ui::open_link`, which opens only http and https); links inside the document scroll to their place. Nothing navigates
 * the viewer page itself.
 */
export function linkService(document: PdfDocument, go: (index: number, left?: number, top?: number) => void, current: () => number): PdfLinkService {
  const coordinate = (value: unknown): number | undefined => typeof value === 'number' ? value : undefined
  return {
    addLinkAttributes(link, url) {
      link.title = url
      if (!/^https?:\/\//i.test(url)) return
      link.href = url
      link.rel = 'noopener noreferrer nofollow'
      link.onclick = (event) => {
        event.preventDefault()
        window.open(url, '_blank')
        return false
      }
    },
    getDestinationHash: () => '',
    getAnchorUrl: () => '',
    async goToDestination(destination) {
      try {
        const explicit = typeof destination === 'string' ? await document.getDestination(destination) : destination
        if (!Array.isArray(explicit)) return
        const [ref, kind, ...args] = explicit as unknown[]
        const index = typeof ref === 'number' ? ref : typeof ref === 'object' && ref !== null ? await document.getPageIndex(ref) : -1
        if (!Number.isInteger(index) || index < 0 || index >= document.numPages) return
        const name = typeof kind === 'object' && kind !== null && 'name' in kind ? String(kind.name) : ''
        // [page /XYZ left top zoom], [page /FitH top], [page /FitBH top], [page /FitR left bottom right top]; the rest show the page.
        if (name === 'XYZ') go(index, coordinate(args[0]), coordinate(args[1]))
        else if (name === 'FitH' || name === 'FitBH') go(index, undefined, coordinate(args[0]))
        else if (name === 'FitR') go(index, coordinate(args[0]), coordinate(args[3]))
        else go(index)
      } catch {
        // A destination the document does not resolve goes nowhere.
      }
    },
    executeNamedAction(action) {
      const index = current() - 1
      const target = action === 'NextPage' ? index + 1 : action === 'PrevPage' ? index - 1 : action === 'FirstPage' ? 0
        : action === 'LastPage' ? document.numPages - 1 : -1
      if (target >= 0 && target < document.numPages) go(target)
    },
  }
}

/**
 * As in pdf.js's viewer: a block behind the text, from the pointer to the end of the page while the button is down, so a
 * drag past the last line keeps selecting to the end of the page instead of jumping back to the start of the layer.
 * Uses a window-level mouseup listener (returned with a disposer) so releasing outside the page never leaves it stuck active.
 */
export function endOfContent(layer: HTMLElement): { readonly element: HTMLElement; readonly dispose: () => void } {
  const end = document.createElement('div')
  end.className = css.endOfContent
  const onDown = (event: MouseEvent): void => {
    if (event.target !== layer) {
      const bounds = layer.getBoundingClientRect()
      if (bounds.height > 0) {
        end.style.top = `${(Math.max(0, (event.clientY - bounds.top) / bounds.height) * 100).toFixed(2)}%`
      }
    }
    end.dataset.active = ''
  }
  const onUp = (): void => {
    end.style.top = ''
    delete end.dataset.active
  }
  layer.addEventListener('mousedown', onDown)
  window.addEventListener('mouseup', onUp)
  return {
    element: end,
    dispose: () => {
      layer.removeEventListener('mousedown', onDown)
      window.removeEventListener('mouseup', onUp)
    },
  }
}
