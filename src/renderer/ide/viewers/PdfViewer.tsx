/** PDF viewer on the bundled pdf.js 2.16 build (`resources/pdfjs`): pages render on demand with selectable text and links,
 *  a new zoom keeps the reader's place, a password-protected file asks for its password, and the text can be extracted. */
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Button, fileSizeText, isComposingKey, toast, writeClipboard } from '../../ui/index.ts'
import { useViewerT } from './messages.ts'
import { describePdfError, PdfJsUnavailableError } from './pdf-errors.ts'
import { anchoredScroll, canvasPixelRatio, captureAnchor, pageIndexAt, type Anchor, type Box } from './pdf-layout.ts'
import { fixRadicals, joinTextItems, selectedText, type PdfTextItem, type TextLayerRecord } from './pdf-text.ts'
import { ToolActions, useNarrow, type ToolAction } from './ToolActions.tsx'
import { useZoomKeys } from './zoom-keys.ts'
import css from './Viewers.module.css'

/** The parts of the pdf.js 2.16 API this viewer uses. */
interface PdfViewport {
  readonly width: number
  readonly height: number
  clone(options: { dontFlip: boolean }): PdfViewport
  /** PDF user space to CSS pixels of the viewport, y going down. */
  convertToViewportPoint(x: number, y: number): number[]
}
interface PdfTask { readonly promise: Promise<unknown>; cancel(): void }
interface PdfTextContent { readonly items: readonly PdfTextItem[]; readonly styles: Readonly<Record<string, unknown>> }
/** An annotation as `getAnnotations()` gives it; the annotation layer reads the other fields. */
interface PdfAnnotation { readonly annotationType: number }
interface PdfPage {
  getViewport(options: { scale: number }): PdfViewport
  render(options: { canvasContext: CanvasRenderingContext2D; viewport: PdfViewport }): PdfTask
  getTextContent(): Promise<PdfTextContent>
  getAnnotations(): Promise<PdfAnnotation[]>
  /** Frees what the page parsed for rendering once no render of it runs; the next render parses it again. */
  cleanup(): boolean
}
interface PdfDocument {
  readonly numPages: number
  getPage(page: number): Promise<PdfPage>
  getDestination(name: string): Promise<unknown[] | null>
  getPageIndex(ref: unknown): Promise<number>
}
interface PdfLoadingTask {
  readonly promise: Promise<PdfDocument>
  /** Stops loading, or closes the document once it is open. */
  destroy(): Promise<void>
  /** Called instead of failing when the file needs a password, and again with `INCORRECT_PASSWORD` after a wrong one. */
  onPassword: ((update: (password: string) => void, reason: number) => void) | null
}
/** What the annotation layer calls back for links: the part of pdf.js's `PDFLinkService` it uses. */
interface PdfLinkService {
  addLinkAttributes(link: HTMLAnchorElement, url: string, newWindow?: boolean): void
  getDestinationHash(destination: unknown): string
  getAnchorUrl(hash: string): string
  goToDestination(destination: unknown): Promise<void>
  executeNamedAction(action: string): void
}
interface PdfJs {
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

interface Size { readonly width: number; readonly height: number }

const PDFJS_ROOT = '/rainy/pdfjs/'
/** pdf.js measures pages in points; 100% shows a page at its printed size on a 96 dpi screen. */
const PDF_TO_CSS = 96 / 72
/** Automatic zoom fits the page width but stops here, as in pdf.js. */
const MAX_AUTO_ZOOM = 1.25
const MIN_ZOOM = 0.25
const MAX_ZOOM = 5
/** One Ctrl+wheel notch zooms by this factor, as in pdf.js's viewer. A touchpad pinch sends a stream of wheel events, so
 *  steps closer together than `WHEEL_INTERVAL` ms are dropped. */
const WHEEL_STEP = 1.1
const WHEEL_INTERVAL = 40
/** pdf.js's `AnnotationType.LINK`, which its 2.16 build does not export. */
const LINK_ANNOTATION = 2
/** Zoom buttons, keys and resizing hold the top centre of the view in place. */
const HOLD_TOP = { x: 0.5, y: 0 }
let loading: Promise<PdfJs> | undefined

/** @returns pdf.js, loaded once from the resources the Host serves. */
function loadPdfJs(): Promise<PdfJs> {
  loading ??= new Promise<PdfJs>((resolve, reject) => {
    const global = globalThis as { pdfjsLib?: PdfJs }
    if (global.pdfjsLib !== undefined) { resolve(global.pdfjsLib); return }
    const script = document.createElement('script')
    script.src = `${PDFJS_ROOT}pdf.min.js`
    script.onload = () => {
      if (global.pdfjsLib === undefined) { reject(new PdfJsUnavailableError('pdf.js did not load')); return }
      global.pdfjsLib.GlobalWorkerOptions.workerSrc = `${PDFJS_ROOT}pdf.worker.min.js`
      resolve(global.pdfjsLib)
    }
    script.onerror = () => { reject(new PdfJsUnavailableError('pdf.js is unavailable')) }
    document.head.append(script)
  }).catch((error: unknown) => { loading = undefined; throw error })
  return loading
}

/** What the pages of one open document share. */
interface PdfContext {
  readonly library: PdfJs
  readonly document: PdfDocument
  readonly links: PdfLinkService
  /** Text layers of the rendered pages by page number, for copying a selection. */
  readonly layers: Map<number, TextLayerRecord>
}

/**
 * Links of the annotation layer. Web addresses open through `window.open`, which the desktop hands to the system browser
 * (`ui::open_link`, which opens only http and https); links inside the document scroll to their place. Nothing navigates
 * the viewer page itself.
 * @param document The open document.
 * @param go Scrolls to page `index`, to a point of it in PDF user space when given.
 * @param current The page number the indicator shows.
 */
function linkService(document: PdfDocument, go: (index: number, left?: number, top?: number) => void, current: () => number): PdfLinkService {
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

/** As in pdf.js's viewer: a block behind the text, from the pointer to the end of the page while the button is down, so a
 *  drag past the last line keeps selecting to the end of the page instead of jumping back to the start of the layer. */
function endOfContent(layer: HTMLElement): HTMLElement {
  const end = document.createElement('div')
  end.className = css.endOfContent
  layer.addEventListener('mousedown', (event) => {
    if (event.target !== layer) {
      const bounds = layer.getBoundingClientRect()
      end.style.top = `${(Math.max(0, (event.clientY - bounds.top) / bounds.height) * 100).toFixed(2)}%`
    }
    end.dataset.active = ''
  })
  layer.addEventListener('mouseup', () => {
    end.style.top = ''
    delete end.dataset.active
  })
  return end
}

/** One page: a frame of the page's size that holds a canvas, the text layer and the links while it is near the view. */
function PdfPageView({ context, number, size, scale, root }: {
  context: PdfContext
  number: number
  size: Size
  scale: number
  root: HTMLElement | null
}): JSX.Element {
  const frame = useRef<HTMLDivElement>(null)
  const [visible, setVisible] = useState(false)
  /** This page's own size once it has loaded; pages may differ from the first. */
  const [own, setOwn] = useState<Size | undefined>()
  /** What the frame shows, and the scale it was made for. The canvas stays while another zoom renders, stretched by CSS,
   *  so the page does not flash blank. */
  const shown = useRef<{ canvas?: HTMLCanvasElement | undefined; text?: HTMLElement | undefined; links?: HTMLElement | undefined; scale?: number | undefined }>({})
  /** Whether pdf.js holds resources it parsed to render this page, which `cleanup()` gives back. */
  const parsed = useRef(false)

  const dropLayers = (): void => {
    shown.current.text?.remove()
    shown.current.links?.remove()
    shown.current = { canvas: shown.current.canvas, scale: shown.current.scale }
    context.layers.delete(number)
  }
  /** A page far from the view keeps only its empty frame: canvas pixels, layers and pdf.js's parsed resources go. */
  const release = (): void => {
    dropLayers()
    const canvas = shown.current.canvas
    if (canvas !== undefined) {
      canvas.remove()
      canvas.width = 0
      canvas.height = 0
    }
    shown.current = {}
    if (parsed.current) {
      parsed.current = false
      void context.document.getPage(number).then((page) => { page.cleanup() }).catch(() => undefined)
    }
  }
  const latestRelease = useRef(release)
  latestRelease.current = release
  useEffect(() => () => { latestRelease.current() }, [])

  useEffect(() => {
    const element = frame.current
    if (element === null) return
    const observer = new IntersectionObserver(entries => { setVisible(entries.some(entry => entry.isIntersecting)) }, { root, rootMargin: '600px 0px' })
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [root])

  useEffect(() => {
    const element = frame.current
    if (element === null) return
    if (!visible) { release(); return }
    let cancelled = false
    let kept = false
    const tasks: PdfTask[] = []
    const fresh = document.createElement('canvas')
    // Text and links are placed for one zoom; at another they would sit beside the words until the new ones are ready.
    if (shown.current.scale !== scale) dropLayers()
    parsed.current = true
    void context.document.getPage(number).then(async (page) => {
      if (cancelled) return
      const base = page.getViewport({ scale: 1 })
      setOwn(previous => previous?.width === base.width && previous.height === base.height ? previous : { width: base.width, height: base.height })
      const viewport = page.getViewport({ scale })
      const pixels = page.getViewport({ scale: scale * canvasPixelRatio(viewport.width, viewport.height, window.devicePixelRatio || 1) })
      fresh.width = Math.max(1, Math.floor(pixels.width))
      fresh.height = Math.max(1, Math.floor(pixels.height))
      const canvasContext = fresh.getContext('2d', { alpha: false })
      if (canvasContext === null) return
      const text = page.getTextContent().catch(() => undefined)
      const annotations = page.getAnnotations().catch(() => [])
      const render = page.render({ canvasContext, viewport: pixels })
      tasks.push(render)
      await render.promise
      if (cancelled) return
      const old = shown.current.canvas
      if (old === undefined) {
        element.prepend(fresh)
      } else {
        old.replaceWith(fresh)
        old.width = 0
        old.height = 0
      }
      kept = true
      shown.current = { ...shown.current, canvas: fresh, scale }

      const content = await text
      if (cancelled || content === undefined) return
      // The layer shows the same text 提取文本 copies, ideographs in place of radicals; the mapping keeps every length.
      const items = content.items.map(item => item.str === undefined ? item : { ...item, str: fixRadicals(item.str) })
      const layer = document.createElement('div')
      layer.className = css.textLayer
      layer.style.width = `${Math.floor(viewport.width)}px`
      layer.style.height = `${Math.floor(viewport.height)}px`
      const divs: HTMLElement[] = []
      const textTask = context.library.renderTextLayer({ textContent: { items, styles: content.styles }, container: layer, viewport, textDivs: divs })
      tasks.push(textTask)
      await textTask.promise
      if (cancelled) return
      layer.append(endOfContent(layer))
      shown.current.text?.remove()
      element.append(layer)
      shown.current = { ...shown.current, text: layer }
      // pdf.js makes one text div for every item that has a string, in order.
      context.layers.set(number, { divs, items: items.filter(item => item.str !== undefined) })

      const links = (await annotations).filter(annotation => annotation.annotationType === LINK_ANNOTATION)
      if (cancelled || links.length === 0) return
      const div = document.createElement('div')
      div.className = css.linkLayer
      div.style.setProperty('--scale-factor', String(scale))
      try {
        context.library.AnnotationLayer.render({ viewport: viewport.clone({ dontFlip: true }), div, annotations: links, page, linkService: context.links, renderForms: false })
      } catch {
        return
      }
      // A link inside the document keeps no address, so Ctrl+click or a middle click cannot open the viewer page itself in
      // the browser; its click handler still scrolls.
      for (const anchor of div.querySelectorAll('a')) {
        if (!/^https?:/i.test(anchor.getAttribute('href') ?? '')) anchor.removeAttribute('href')
      }
      shown.current.links?.remove()
      element.append(div)
      shown.current = { ...shown.current, links: div }
    }).catch(() => undefined)
    return () => {
      cancelled = true
      for (const task of tasks) task.cancel()
      if (!kept) {
        fresh.width = 0
        fresh.height = 0
      }
    }
  }, [visible, context, number, scale])

  return (
    <div ref={frame} className={`${css.page} ${css.pdfPage}`} data-page={number}
      style={{ width: Math.round((own ?? size).width * scale), height: Math.round((own ?? size).height * scale) }} />
  )
}

/** The password prompt of a protected PDF. The password goes from the input straight to pdf.js: it is never kept in
 *  state, stored or logged, and the input is emptied as it is handed over. */
function PdfPassword({ incorrect, onSubmit }: { incorrect: boolean; onSubmit: (password: string) => void }): JSX.Element {
  const t = useViewerT()
  const input = useRef<HTMLInputElement>(null)
  const hint = useId()
  const submit = (): void => {
    const field = input.current
    if (field === null) return
    if (field.value === '') { field.focus(); return }
    const password = field.value
    field.value = ''
    onSubmit(password)
  }
  return (
    <div className={`${css.message} ${css.pdfPassword}`}>
      <p id={hint} className={incorrect ? css.error : undefined} role={incorrect ? 'alert' : undefined}>{t(incorrect ? 'pdfPasswordWrong' : 'pdfPasswordNeeded')}</p>
      <div className={css.pdfPasswordRow}>
        <input ref={input} type="password" className={css.pdfPasswordInput} autoFocus autoComplete="off" spellCheck={false}
          aria-label={t('pdfPasswordLabel')} placeholder={t('pdfPasswordLabel')} aria-describedby={hint} aria-invalid={incorrect || undefined}
          onKeyDown={(event) => {
            if (event.key !== 'Enter' || isComposingKey(event.nativeEvent)) return
            event.preventDefault()
            submit()
          }} />
        <Button variant="primary" size="sm" onClick={submit}>{t('pdfPasswordOpen')}</Button>
      </div>
    </div>
  )
}

/**
 * Ctrl+wheel zooms around the pointer; Ctrl+= and Ctrl+- step the zoom like the buttons, and Ctrl+0 goes back to
 * automatic zoom, the zoom a PDF opens at.
 * @param props.url Byte route URL of the PDF.
 * @param props.data The PDF itself, for a copy made from another format; it takes the place of `url`.
 * @param props.bytes File size shown in the status bar.
 * @param props.modes Toolbar items before the zoom controls, such as a choice between previews.
 * @param props.actions Secondary actions after 提取文本, folded with it into 更多 on a narrow page.
 * @param props.tools Extra toolbar items at the end.
 * @param props.note Status bar text before the size.
 * @returns The viewer.
 */
export function PdfViewer({ url, data, bytes, modes, actions, tools, note }: {
  url?: string | undefined
  data?: Uint8Array | undefined
  bytes: number
  modes?: ReactNode
  actions?: readonly ToolAction[] | undefined
  tools?: ReactNode
  note?: string | undefined
}): JSX.Element {
  const t = useViewerT()
  const narrow = useNarrow()
  const stage = useRef<HTMLDivElement>(null)
  const frames = useRef<HTMLDivElement>(null)
  const [opened, setOpened] = useState<{ library: PdfJs; document: PdfDocument } | undefined>()
  const [size, setSize] = useState<Size>({ width: 612, height: 792 })
  const [zoom, setZoom] = useState<number | 'auto'>('auto')
  const [width, setWidth] = useState(800)
  const [page, setPage] = useState(1)
  const [failure, setFailure] = useState<{ error: unknown } | undefined>()
  const [password, setPassword] = useState<{ submit: (password: string) => void; incorrect: boolean; attempt: number } | undefined>()
  const [extracting, setExtracting] = useState(false)
  /** The reader's place before the latest layout change, kept current while scrolling. */
  const place = useRef<Anchor | undefined>(undefined)
  /** The place a zoom action holds instead, such as the point under the pointer for Ctrl+wheel. */
  const held = useRef<Anchor | undefined>(undefined)
  /** The scale the pages were last laid out at. */
  const laidOut = useRef<number | undefined>(undefined)
  const lastWheel = useRef(0)
  const currentPage = useRef(page)
  currentPage.current = page

  useEffect(() => {
    let cancelled = false
    let task: PdfLoadingTask | undefined
    setOpened(undefined); setFailure(undefined); setPassword(undefined); setPage(1)
    place.current = undefined
    held.current = undefined
    void loadPdfJs().then(async (library) => {
      if (cancelled) return
      // isEvalSupported stays off: pdf.js before 4.2.67 can run script from a crafted font otherwise (CVE-2024-4367).
      // pdf.js hands `data` to its worker, which detaches the buffer, so it gets a copy.
      const opening = library.getDocument({ ...data === undefined ? { url } : { data: data.slice() }, isEvalSupported: false, enableXfa: false, cMapUrl: `${PDFJS_ROOT}cmaps/`, cMapPacked: true,
        standardFontDataUrl: `${PDFJS_ROOT}standard_fonts/` })
      task = opening
      let attempt = 0
      opening.onPassword = (update, reason) => {
        if (cancelled) return
        attempt += 1
        setPassword({ submit: (value) => { setPassword(undefined); update(value) }, incorrect: reason === library.PasswordResponses.INCORRECT_PASSWORD, attempt })
      }
      const document = await opening.promise
      if (cancelled) return
      const first = (await document.getPage(1)).getViewport({ scale: 1 })
      if (cancelled) return
      setSize({ width: first.width, height: first.height })
      setOpened({ library, document })
    }).catch((error: unknown) => {
      if (cancelled) return
      setPassword(undefined)
      setFailure({ error })
    })
    // Destroying the loading task also closes the document it opened.
    return () => { cancelled = true; void task?.destroy().catch(() => undefined) }
  }, [url, data])

  useEffect(() => {
    const element = stage.current
    if (element === null) return
    const observer = new ResizeObserver(() => { setWidth(element.clientWidth) })
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [])

  const autoFactor = Math.max(0.1, Math.min(MAX_AUTO_ZOOM, (width - 48) / (size.width * PDF_TO_CSS)))
  const factor = zoom === 'auto' ? autoFactor : zoom
  const scale = factor * PDF_TO_CSS
  const count = opened?.document.numPages ?? 0
  const pages = useMemo(() => Array.from({ length: count }, (_unused, index) => index + 1), [count])

  /** Frame of page `index` as laid out, in the stage's content coordinates: the stage is the frames' offset parent. */
  const frameBox = (index: number): Box => {
    const frame = frames.current?.children[index] as HTMLElement | undefined
    return frame === undefined ? { top: 0, left: 0, width: 0, height: 0 } : { top: frame.offsetTop, left: frame.offsetLeft, width: frame.offsetWidth, height: frame.offsetHeight }
  }
  const capture = (at = HOLD_TOP): Anchor | undefined => {
    const element = stage.current
    const list = frames.current
    if (element === null || list === null) return undefined
    return captureAnchor(list.children.length, frameBox, { left: element.scrollLeft, top: element.scrollTop, width: element.clientWidth, height: element.clientHeight }, at)
  }
  /** The indicator names the page at a third of the view's height, found among the frames as laid out, whatever their sizes. */
  const updatePage = (): void => {
    const element = stage.current
    const list = frames.current
    if (element === null || list === null || list.children.length === 0) return
    setPage(pageIndexAt(list.children.length, index => frameBox(index).top, element.scrollTop + element.clientHeight / 3) + 1)
  }
  const onScroll = (): void => {
    place.current = capture()
    updatePage()
  }

  // A new scale lays every page out again; the held place goes back where it was in the view before the browser paints.
  useLayoutEffect(() => {
    const previous = laidOut.current
    laidOut.current = scale
    const anchor = held.current ?? place.current
    held.current = undefined
    const element = stage.current
    if (element === null || opened === undefined || previous === undefined || previous === scale || anchor === undefined) return
    const target = anchoredScroll(anchor, frames.current?.children.length ?? 0, frameBox, { width: element.clientWidth, height: element.clientHeight })
    if (target === undefined) return
    element.scrollLeft = target.left
    element.scrollTop = target.top
    place.current = capture()
    updatePage()
  }, [scale, opened])

  const zoomTo = (next: number | 'auto', at?: { x: number; y: number }): void => {
    if (Math.abs((next === 'auto' ? autoFactor : next) - factor) > 1e-6) held.current = capture(at)
    setZoom(next)
  }
  /** `next` within the zoom range, never moving against `direction`: below the smallest zoom (an automatic zoom in a
   *  narrow pane) zooming out keeps the zoom rather than enlarging it. */
  const toward = (direction: 1 | -1, next: number): number => {
    const bounded = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, next))
    return direction > 0 ? Math.max(bounded, factor) : Math.min(bounded, factor)
  }
  const step = (direction: 1 | -1): void => { zoomTo(toward(direction, Math.round((factor + direction * 0.25) * 4) / 4)) }
  const wheel = (direction: 1 | -1, clientX: number, clientY: number): void => {
    const element = stage.current
    const now = performance.now()
    if (element === null || now - lastWheel.current < WHEEL_INTERVAL) return
    lastWheel.current = now
    const bounds = element.getBoundingClientRect()
    const next = toward(direction, Math.round(factor * (direction > 0 ? WHEEL_STEP : 1 / WHEEL_STEP) * 100) / 100)
    zoomTo(next, { x: (clientX - bounds.left - element.clientLeft) / element.clientWidth, y: (clientY - bounds.top - element.clientTop) / element.clientHeight })
  }
  useZoomKeys(stage.current, {
    zoomIn: (x, y) => { if (x === undefined || y === undefined) step(1); else wheel(1, x, y) },
    zoomOut: (x, y) => { if (x === undefined || y === undefined) step(-1); else wheel(-1, x, y) },
    reset: () => { zoomTo('auto') },
  }, opened !== undefined)

  /** Scrolls to page `index`, to a point of it in PDF user space when `top` is given. */
  const goTo = useRef<(index: number, left?: number, top?: number) => void>(() => undefined)
  goTo.current = (index, left, top) => {
    const element = stage.current
    const frame = frames.current?.children[index] as HTMLElement | undefined
    if (element === null || frame === undefined || opened === undefined) return
    if (top === undefined) { element.scrollTop = frame.offsetTop; return }
    void opened.document.getPage(index + 1).then((target) => {
      const point = target.getViewport({ scale }).convertToViewportPoint(left ?? 0, top)
      element.scrollTop = frame.offsetTop + Math.max(0, point[1] ?? 0)
    }).catch(() => undefined)
  }
  const context = useMemo<PdfContext | undefined>(() => opened === undefined ? undefined : {
    library: opened.library,
    document: opened.document,
    links: linkService(opened.document, (index, left, top) => { goTo.current(index, left, top) }, () => currentPage.current),
    layers: new Map(),
  }, [opened])

  // Ctrl+C on a selection in the pages copies the text as 提取文本 joins it. The listener is on the document because the
  // copy event of a selection outside an editable element may go to the body rather than to the selected text.
  useEffect(() => {
    if (context === undefined) return
    const onCopy = (event: ClipboardEvent): void => {
      const selection = window.getSelection()
      if (event.defaultPrevented || event.clipboardData === null || selection === null || selection.rangeCount === 0 || selection.isCollapsed) return
      const layers = [...context.layers].sort(([a], [b]) => a - b).map(([, layer]) => layer)
      const text = selectedText(selection.getRangeAt(0), layers)
      if (text === undefined) return
      event.preventDefault()
      event.clipboardData.setData('text/plain', text)
    }
    document.addEventListener('copy', onCopy)
    return () => { document.removeEventListener('copy', onCopy) }
  }, [context])
  const extract = async (): Promise<void> => {
    if (opened === undefined) return
    setExtracting(true)
    try {
      const texts: string[] = []
      for (const number of pages) {
        const content = await (await opened.document.getPage(number)).getTextContent()
        texts.push(`--- ${number} ---\n${joinTextItems(content.items)}`)
      }
      const text = texts.join('\n\n')
      if (await writeClipboard(text, { quiet: true })) toast(t('pdfTextCopied', { count: text.length }), { tone: 'success' })
      else toast(t('copyFailed'), { tone: 'error' })
    } catch (error) {
      const problem = describePdfError(error)
      toast(t('pdfTextFailed', { error: t(problem.key, problem.vars) }), { tone: 'error' })
    } finally { setExtracting(false) }
  }

  const problem = failure === undefined ? undefined : describePdfError(failure.error)
  const pageText = t('pdfPage', { page, pages: count })
  return (
    <div className={css.viewer} data-viewer="pdf">
      <div className={css.toolbar}>
        {modes !== undefined && <>{modes}<span className={css.separator} /></>}
        <button type="button" className={css.toolButton} aria-label={t('zoomOut')} title={t('zoomOut')} onClick={() => { step(-1) }}>−</button>
        <span className={css.pdfZoomLevel}>{t('zoomLevel', { percent: Math.round(factor * 100) })}</span>
        <button type="button" className={css.toolButton} aria-label={t('zoomIn')} title={t('zoomIn')} onClick={() => { step(1) }}>+</button>
        {!narrow && <button type="button" className={css.toolButton} aria-pressed={zoom === 'auto'} onClick={() => { zoomTo('auto') }}>{t('zoomAuto')}</button>}
        {opened !== undefined && <>
          {!narrow && <span className={css.separator} />}
          <span title={pageText}>{narrow ? t('pdfPageShort', { page, pages: count }) : pageText}</span>
        </>}
        <span className={css.grow} />
        <ToolActions actions={[
          narrow && { id: 'pdf-zoom-auto', label: t('zoomAuto'), disabled: zoom === 'auto', onSelect: () => { zoomTo('auto') } },
          { id: 'pdf-text', label: t('pdfText'), title: t('pdfTextTitle'), disabled: opened === undefined || extracting, onSelect: () => { void extract() } },
          ...actions ?? [],
        ]} />
        {tools}
      </div>
      <div ref={stage} className={css.stage} onScroll={onScroll}>
        {problem !== undefined ? (
          <div className={css.message}>
            <span className={css.error} title={problem.detail}>{t('pdfFailed', { error: t(problem.key, problem.vars) })}</span>
            {problem.detailed && <span className={css.pdfErrorDetail}>{problem.detail}</span>}
          </div>
        ) : password !== undefined ? <PdfPassword key={password.attempt} incorrect={password.incorrect} onSubmit={password.submit} />
          : context === undefined ? <div className={css.message}>{t('loading')}</div>
            : <div ref={frames} className={`${css.pages} ${css.pdfPages}`}>{pages.map(number => (
              <PdfPageView key={number} context={context} number={number} size={size} scale={scale} root={stage.current} />
            ))}</div>}
      </div>
      <div className={css.status}>{note !== undefined && <span>{note}</span>}<span>{fileSizeText(bytes)}</span></div>
    </div>
  )
}
