/** PDF viewer on the bundled pdf.js 2.16 build (`resources/pdfjs`): pages render on demand with selectable text and links,
 *  a new zoom keeps the reader's place, a password-protected file asks for its password, the text can be extracted, and
 *  Ctrl+F finds text in every page. */
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Button, fileSizeText, IconAction, IconChevronDownOutlineRegular, IconChevronUpOutlineRegular, IconCloseOutlineRegular, isComposingKey, toast,
  writeClipboard } from '../../ui/index.ts'
import { useViewerT } from './messages.ts'
import { describePdfError } from './pdf-errors.ts'
import { findMatchesInPage, findPattern, MAX_MATCHES, PdfTextCache, type MatchPiece, type PdfMatch } from './pdf-find.ts'
import { anchoredScroll, canvasPixelRatio, captureAnchor, pageIndexAt, type Anchor, type Box } from './pdf-layout.ts'
import { endOfContent, linkService, loadPdfJs, PDFJS_ROOT, type PdfContext, type PdfDocument, type PdfJs, type PdfLoadingTask, type PdfTask } from './pdf-runtime.ts'
import { fixRadicals, joinTextItems, layOutText, selectedText } from './pdf-text.ts'
import { ToolActions, useNarrow, type ToolAction } from './ToolActions.tsx'
import { useZoomKeys } from './zoom-keys.ts'
import css from './Viewers.module.css'

interface Size { readonly width: number; readonly height: number }

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

/** The matches of the find bar on one page: the item pieces of each, which of them is the current one (-1 for none), and
 *  a number that changes when the current one should be scrolled into view. */
interface PageFinds { readonly matches: readonly (readonly MatchPiece[])[]; readonly current: number; readonly reveal: number }

/** One page: a frame of the page's size that holds a canvas, the text layer and the links while it is near the view. */
function PdfPageView({ context, number, size, scale, root, finds }: {
  context: PdfContext
  number: number
  size: Size
  scale: number
  root: HTMLElement | null
  finds: PageFinds | undefined
}): JSX.Element {
  const frame = useRef<HTMLDivElement>(null)
  const [visible, setVisible] = useState(false)
  const visibleRef = useRef(false)
  visibleRef.current = visible
  /** This page's own size once it has loaded; pages may differ from the first. */
  const [own, setOwn] = useState<Size | undefined>()
  /** What the frame shows, and the scale it was made for. The canvas stays while another zoom renders, stretched by CSS,
   *  so the page does not flash blank. */
  const shown = useRef<{ canvas?: HTMLCanvasElement | undefined; text?: HTMLElement | undefined; endCleanup?: (() => void) | undefined; links?: HTMLElement | undefined; scale?: number | undefined }>({})
  /** Whether pdf.js holds resources it parsed to render this page, which `cleanup()` gives back. */
  const parsed = useRef(false)
  /** Tracks the in-flight render promise so `page.cleanup()` never races an active pdf.js render task. */
  const activeRender = useRef<Promise<unknown>>(Promise.resolve())
  /** Bumped when a new text layer is shown, so the find marks are placed over it. */
  const [layerVersion, setLayerVersion] = useState(0)
  const marks = useRef<HTMLElement | undefined>(undefined)
  const revealed = useRef(0)

  const dropLayers = (): void => {
    shown.current.endCleanup?.()
    shown.current.text?.remove()
    shown.current.links?.remove()
    marks.current?.remove()
    marks.current = undefined
    shown.current = { canvas: shown.current.canvas, scale: shown.current.scale }
    context.layers.delete(number)
  }

  // The find bar's matches, as boxes behind the text layer's own text, measured from it at its current zoom.
  useEffect(() => {
    marks.current?.remove()
    marks.current = undefined
    const element = frame.current
    const record = context.layers.get(number)
    if (element === null || record === undefined || finds === undefined || finds.matches.length === 0) return
    const box = element.getBoundingClientRect()
    const layer = document.createElement('div')
    layer.className = css.findLayer
    let current: HTMLElement | undefined
    finds.matches.forEach((pieces, index) => {
      for (const piece of pieces) {
        const node = record.divs[piece.item]?.firstChild
        if (!(node instanceof Text)) continue
        const range = document.createRange()
        range.setStart(node, Math.min(piece.from, node.length))
        range.setEnd(node, Math.min(piece.to, node.length))
        for (const rect of range.getClientRects()) {
          const mark = document.createElement('div')
          mark.className = css.findMatch
          if (index === finds.current) {
            mark.dataset.current = ''
            current ??= mark
          }
          mark.style.left = `${rect.left - box.left}px`
          mark.style.top = `${rect.top - box.top}px`
          mark.style.width = `${rect.width}px`
          mark.style.height = `${rect.height}px`
          layer.append(mark)
        }
      }
    })
    element.append(layer)
    marks.current = layer
    if (current !== undefined && finds.reveal !== revealed.current) {
      revealed.current = finds.reveal
      current.scrollIntoView({ block: 'center', inline: 'nearest' })
    }
  }, [finds, layerVersion, context, number])

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
      const settle = activeRender.current.catch(() => undefined)
      void settle.then(() => {
        if (visibleRef.current) return
        return context.document.getPage(number).then((page) => { page.cleanup() })
      }).catch(() => undefined)
    }
  }
  const latestRelease = useRef(release)
  latestRelease.current = release
  useEffect(() => () => { visibleRef.current = false; latestRelease.current() }, [])

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
    const run = context.document.getPage(number).then(async (page) => {
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
      const eoc = endOfContent(layer)
      layer.append(eoc.element)
      shown.current.endCleanup?.()
      shown.current.text?.remove()
      element.append(layer)
      shown.current = { ...shown.current, text: layer, endCleanup: eoc.dispose }
      // pdf.js makes one text div for every item that has a string, in order.
      context.layers.set(number, { divs, items: items.filter(item => item.str !== undefined) })
      setLayerVersion(value => value + 1)

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
    activeRender.current = run
    return () => {
      cancelled = true
      for (const task of tasks) task.cancel()
      if (!kept) {
        fresh.width = 0
        fresh.height = 0
      }
    }
  // `release` reads only refs and stable props.
  // eslint-disable-next-line react-hooks/exhaustive-deps
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

/** What the find bar found: every match in reading order, the current one, whether results reached MAX_MATCHES, and a
 *  number that changes when it should be scrolled into view. */
interface Found { readonly matches: readonly PdfMatch[]; readonly current: number; readonly reveal: number; readonly capped: boolean }

/** The find bar (Ctrl+F): the query, how many matches, the previous and next match, and whether case counts. */
function PdfFindBar({ request, query, caseSensitive, found, searching, onQuery, onCase, onStep, onClose }: {
  /** Changes with every Ctrl+F, which selects the query again. */
  request: number
  query: string
  caseSensitive: boolean
  found: Found | undefined
  searching: boolean
  onQuery: (query: string) => void
  onCase: () => void
  onStep: (delta: 1 | -1) => void
  onClose: () => void
}): JSX.Element {
  const t = useViewerT()
  const input = useRef<HTMLInputElement>(null)
  useEffect(() => { input.current?.focus(); input.current?.select() }, [request])
  const total = found?.matches.length ?? 0
  const totalLabel = found?.capped === true ? `${total}+` : total
  const count = query.trim() === '' ? '' : searching ? t('pdfFindSearching') : total === 0 ? t('findNone') : t('findCount', { current: (found?.current ?? 0) + 1, total: totalLabel })
  return <div className="rainy-find" role="search" data-pdf-find onKeyDown={(event) => {
    if (event.key !== 'Escape') return
    event.preventDefault()
    event.stopPropagation()
    onClose()
  }}>
    <div className="rainy-find-row">
      <input ref={input} className="rainy-find-input" value={query} placeholder={t('findPlaceholder')} aria-label={t('findPlaceholder')} spellCheck={false}
        onChange={(event) => { onQuery(event.target.value) }}
        onKeyDown={(event) => {
          if (event.key !== 'Enter' || isComposingKey(event.nativeEvent)) return
          event.preventDefault()
          onStep(event.shiftKey ? -1 : 1)
        }} />
      <span className="rainy-find-count" aria-live="polite">{count}</span>
      <button type="button" className="rainy-find-toggle" aria-pressed={caseSensitive} title={t('findCase')} aria-label={t('findCase')} onClick={onCase}>Aa</button>
      <IconAction label={t('findPrevious')} disabled={total === 0} onClick={() => { onStep(-1) }}><IconChevronUpOutlineRegular size={14} /></IconAction>
      <IconAction label={t('findNext')} disabled={total === 0} onClick={() => { onStep(1) }}><IconChevronDownOutlineRegular size={14} /></IconAction>
      <IconAction label={t('findClose')} onClick={onClose}><IconCloseOutlineRegular size={14} /></IconAction>
    </div>
  </div>
}

/**
 * Ctrl+wheel zooms around the pointer; Ctrl+= and Ctrl+- step the zoom like the buttons, and Ctrl+0 goes back to
 * automatic zoom, the zoom a PDF opens at. Ctrl+F finds text: Enter and F3 go to the next match, Shift with them to the
 * previous one, Esc closes the bar.
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
  const mounted = useRef(true)
  useEffect(() => {
    mounted.current = true
    return () => { mounted.current = false }
  }, [])
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
      if (document.numPages > 0) {
        const first = (await document.getPage(1)).getViewport({ scale: 1 })
        if (cancelled) return
        setSize({ width: first.width, height: first.height })
      } else {
        setPage(0)
      }
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
    const anchor = element.clientWidth > 0 && element.clientHeight > 0
      ? { x: (clientX - bounds.left - element.clientLeft) / element.clientWidth, y: (clientY - bounds.top - element.clientTop) / element.clientHeight }
      : { x: 0.5, y: 0.5 }
    zoomTo(next, anchor)
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
  // Find: bounded LRU cache of laid-out page text, streamed page-by-page so large PDFs stay within a fixed memory ceiling.
  const [findRequest, setFindRequest] = useState<number | undefined>()
  const [query, setQuery] = useState('')
  const [caseSensitive, setCaseSensitive] = useState(false)
  const [found, setFound] = useState<Found | undefined>()
  const [searching, setSearching] = useState(false)
  const texts = useRef<{ document: PdfDocument | undefined; pages: PdfTextCache }>({ document: undefined, pages: new PdfTextCache() })
  useEffect(() => () => { texts.current.pages.clear(); texts.current.document = undefined }, [])
  const finding = findRequest !== undefined && opened !== undefined
  useEffect(() => {
    const pattern = finding ? findPattern(query, caseSensitive) : undefined
    if (opened === undefined || pattern === undefined) { setFound(undefined); setSearching(false); return }
    if (texts.current.document !== opened.document) {
      texts.current.pages.clear()
      texts.current.document = opened.document
    }
    const cache = texts.current.pages
    let cancelled = false
    setSearching(true)
    const timer = window.setTimeout(() => {
      void (async () => {
        const matches: PdfMatch[] = []
        let capped = false
        for (let number = 1; number <= opened.document.numPages; number++) {
          let text = cache.get(number)
          if (text === undefined) {
            const pdfPage = await opened.document.getPage(number)
            const content = await pdfPage.getTextContent()
            if (cancelled) return
            text = layOutText(content.items)
            cache.set(number, text)
            // Free pdf.js parsed page structures for off-screen pages scanned solely for search.
            if (context?.layers.has(number) !== true) pdfPage.cleanup()
          }
          if (cancelled) return
          if (findMatchesInPage(number, text, pattern, matches, MAX_MATCHES)) { capped = true; break }
        }
        // The first match is the first one on or after the page being read.
        const first = Math.max(0, matches.findIndex(match => match.page >= currentPage.current))
        setFound(previous => ({ matches, current: first, reveal: (previous?.reveal ?? 0) + 1, capped }))
        setSearching(false)
      })().catch(() => {
        if (cancelled) return
        setFound(previous => ({ matches: [], current: 0, reveal: previous?.reveal ?? 0, capped: false }))
        setSearching(false)
      })
    }, 150)
    return () => { cancelled = true; window.clearTimeout(timer) }
  }, [finding, query, caseSensitive, opened, context])
  const stepFind = (delta: 1 | -1): void => {
    setFound(previous => previous === undefined || previous.matches.length === 0 ? previous
      : { ...previous, current: (previous.current + delta + previous.matches.length) % previous.matches.length, reveal: previous.reveal + 1 })
  }
  const latestFind = useRef({ finding, stepFind })
  latestFind.current = { finding, stepFind }
  useEffect(() => {
    if (opened === undefined) return
    const onKey = (event: KeyboardEvent): void => {
      if (event.defaultPrevented || isComposingKey(event)) return
      if ((event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === 'f') {
        event.preventDefault()
        const selected = window.getSelection()?.toString().trim() ?? ''
        if (selected !== '' && !selected.includes('\n') && selected.length <= 200) setQuery(selected)
        setFindRequest(previous => (previous ?? 0) + 1)
      } else if (event.key === 'F3' && !event.ctrlKey && !event.altKey && latestFind.current.finding) {
        event.preventDefault()
        latestFind.current.stepFind(event.shiftKey ? -1 : 1)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => { window.removeEventListener('keydown', onKey) }
  }, [opened])
  // A match on a page that has no text layer yet: showing the page renders it, and the page scrolls to the match.
  useEffect(() => {
    const match = found?.matches[found.current]
    if (match !== undefined && context !== undefined && !context.layers.has(match.page)) goTo.current(match.page - 1)
  // Only a new request to show the current match moves the view.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [found?.reveal])
  const pageFinds = useMemo(() => {
    const byPage = new Map<number, PageFinds>()
    if (found === undefined) return byPage
    const lists = new Map<number, MatchPiece[][]>()
    let current: { page: number; index: number } | undefined
    found.matches.forEach((match, index) => {
      const list = lists.get(match.page) ?? []
      if (index === found.current) current = { page: match.page, index: list.length }
      list.push([...match.pieces])
      lists.set(match.page, list)
    })
    for (const [page, matches] of lists) byPage.set(page, { matches, current: current?.page === page ? current.index : -1, reveal: found.reveal })
    return byPage
  }, [found])

  const extract = async (): Promise<void> => {
    if (opened === undefined) return
    const activeDoc = opened.document
    setExtracting(true)
    try {
      const texts: string[] = []
      for (const number of pages) {
        if (!mounted.current) return
        const pdfPage = await activeDoc.getPage(number)
        const content = await pdfPage.getTextContent()
        if (context?.layers.has(number) !== true) pdfPage.cleanup()
        if (!mounted.current) return
        texts.push(`--- ${number} ---\n${joinTextItems(content.items)}`)
      }
      const text = texts.join('\n\n')
      if (!mounted.current) return
      if (await writeClipboard(text, { quiet: true })) toast(t('pdfTextCopied', { count: text.length }), { tone: 'success' })
      else toast(t('copyFailed'), { tone: 'error' })
    } catch (error) {
      if (!mounted.current) return
      const problem = describePdfError(error)
      toast(t('pdfTextFailed', { error: t(problem.key, problem.vars) }), { tone: 'error' })
    } finally {
      if (mounted.current) setExtracting(false)
    }
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
          { id: 'pdf-text', label: t('pdfText'), title: t('pdfTextTitle'), disabled: opened === undefined || extracting || count === 0, onSelect: () => { void extract() } },
          ...actions ?? [],
        ]} />
        {tools}
      </div>
      {finding && <PdfFindBar request={findRequest} query={query} caseSensitive={caseSensitive} found={found} searching={searching}
        onQuery={setQuery} onCase={() => { setCaseSensitive(value => !value) }} onStep={stepFind} onClose={() => { setFindRequest(undefined) }} />}
      <div ref={stage} className={css.stage} onScroll={onScroll}>
        {problem !== undefined ? (
          <div className={css.message}>
            <span className={css.error} title={problem.detail}>{t('pdfFailed', { error: t(problem.key, problem.vars) })}</span>
            {problem.detailed && <span className={css.pdfErrorDetail}>{problem.detail}</span>}
          </div>
        ) : password !== undefined ? <PdfPassword key={password.attempt} incorrect={password.incorrect} onSubmit={password.submit} />
          : context === undefined ? <div className={css.message}>{t('loading')}</div>
            : <div ref={frames} className={`${css.pages} ${css.pdfPages}`}>{pages.map(number => (
              <PdfPageView key={number} context={context} number={number} size={size} scale={scale} root={stage.current} finds={pageFinds.get(number)} />
            ))}</div>}
      </div>
      <div className={css.status}>{note !== undefined && <span>{note}</span>}<span>{fileSizeText(bytes)}</span></div>
    </div>
  )
}
