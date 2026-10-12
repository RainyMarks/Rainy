/** PDF viewer on the pdf.js build that ships with IceSky: pages render on demand, and their text can be extracted. */
import { useEffect, useMemo, useRef, useState } from 'react'
import { fileSizeText, toast, writeClipboard } from '../../ui/index.ts'
import { useViewerT } from './messages.ts'
import css from './Viewers.module.css'

/** The parts of the pdf.js 2.16 API this viewer uses. */
interface PdfPage {
  getViewport(options: { scale: number }): { width: number; height: number }
  render(options: { canvasContext: CanvasRenderingContext2D; viewport: unknown }): { promise: Promise<void>; cancel(): void }
  getTextContent(): Promise<{ items: readonly { str?: string }[] }>
  cleanup(): void
}
interface PdfDocument { numPages: number; getPage(page: number): Promise<PdfPage>; destroy(): Promise<void> }
interface PdfJs {
  GlobalWorkerOptions: { workerSrc: string }
  getDocument(options: Record<string, unknown>): { promise: Promise<PdfDocument>; destroy(): Promise<void> }
}

const PDFJS_ROOT = '/rainy/icesky/js/vendor/pdfjs/'
/** pdf.js measures pages in points; 100% shows a page at its printed size on a 96 dpi screen. */
const PDF_TO_CSS = 96 / 72
/** Automatic zoom fits the page width but stops here, as in pdf.js. */
const MAX_AUTO_ZOOM = 1.25
let loading: Promise<PdfJs> | undefined

/** @returns pdf.js, loaded once from the IceSky resources the Host serves. */
function loadPdfJs(): Promise<PdfJs> {
  loading ??= new Promise<PdfJs>((resolve, reject) => {
    const global = globalThis as { pdfjsLib?: PdfJs }
    if (global.pdfjsLib !== undefined) { resolve(global.pdfjsLib); return }
    const script = document.createElement('script')
    script.src = `${PDFJS_ROOT}pdf.min.js`
    script.onload = () => {
      if (global.pdfjsLib === undefined) { reject(new Error('pdf.js did not load')); return }
      global.pdfjsLib.GlobalWorkerOptions.workerSrc = `${PDFJS_ROOT}pdf.worker.min.js`
      resolve(global.pdfjsLib)
    }
    script.onerror = () => { reject(new Error('pdf.js is unavailable')) }
    document.head.append(script)
  }).catch((error: unknown) => { loading = undefined; throw error })
  return loading
}

function PdfPageView({ document: pdf, number, size, scale, root }: {
  document: PdfDocument
  number: number
  size: { width: number; height: number }
  scale: number
  root: HTMLElement | null
}): JSX.Element {
  const frame = useRef<HTMLDivElement>(null)
  const canvas = useRef<HTMLCanvasElement>(null)
  const [visible, setVisible] = useState(false)
  /** This page's own size once it has loaded; pages may differ from the first. */
  const [own, setOwn] = useState<{ width: number; height: number } | undefined>()
  useEffect(() => {
    const element = frame.current
    if (element === null) return
    const observer = new IntersectionObserver(entries => { setVisible(entries.some(entry => entry.isIntersecting)) }, { root, rootMargin: '600px 0px' })
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [root])
  useEffect(() => {
    const target = canvas.current
    if (!visible || target === null) return
    let cancelled = false
    let task: { cancel(): void } | undefined
    void pdf.getPage(number).then((page) => {
      if (cancelled) return
      const base = page.getViewport({ scale: 1 })
      setOwn(previous => previous?.width === base.width && previous.height === base.height ? previous : { width: base.width, height: base.height })
      const ratio = window.devicePixelRatio || 1
      const viewport = page.getViewport({ scale: scale * ratio })
      target.width = Math.ceil(viewport.width)
      target.height = Math.ceil(viewport.height)
      const context = target.getContext('2d')
      if (context === null) return
      const render = page.render({ canvasContext: context, viewport })
      task = render
      return render.promise.catch(() => undefined)
    })
    return () => {
      cancelled = true
      task?.cancel()
      // Pages far from the viewport give their pixels back.
      target.width = 0
      target.height = 0
    }
  }, [visible, pdf, number, scale])
  return (
    <div ref={frame} className={css.page} data-page={number} style={{ width: Math.round((own ?? size).width * scale), height: Math.round((own ?? size).height * scale) }}>
      <canvas ref={canvas} />
    </div>
  )
}

/**
 * @param props.url Byte route URL of the PDF.
 * @param props.bytes File size.
 * @returns The viewer.
 */
export function PdfViewer({ url, bytes }: { url: string; bytes: number }): JSX.Element {
  const t = useViewerT()
  const stage = useRef<HTMLDivElement>(null)
  const [pdf, setPdf] = useState<PdfDocument | undefined>()
  const [size, setSize] = useState<{ width: number; height: number }>({ width: 612, height: 792 })
  const [zoom, setZoom] = useState<number | 'auto'>('auto')
  const [width, setWidth] = useState(800)
  const [page, setPage] = useState(1)
  const [error, setError] = useState('')
  const [extracting, setExtracting] = useState(false)

  useEffect(() => {
    let cancelled = false
    let opened: PdfDocument | undefined
    setPdf(undefined); setError(''); setPage(1)
    void loadPdfJs().then(async (library) => {
      // isEvalSupported stays off: pdf.js before 4.2.67 can run script from a crafted font otherwise (CVE-2024-4367).
      const task = library.getDocument({ url, isEvalSupported: false, enableXfa: false, cMapUrl: `${PDFJS_ROOT}cmaps/`, cMapPacked: true,
        standardFontDataUrl: `${PDFJS_ROOT}standard_fonts/` })
      const document = await task.promise
      if (cancelled) { void document.destroy(); return }
      opened = document
      const first = (await document.getPage(1)).getViewport({ scale: 1 })
      if (cancelled) return
      setSize({ width: first.width, height: first.height })
      setPdf(document)
    }).catch((failure: unknown) => { if (!cancelled) setError(failure instanceof Error ? failure.message : String(failure)) })
    return () => { cancelled = true; void opened?.destroy() }
  }, [url])

  useEffect(() => {
    const element = stage.current
    if (element === null) return
    const observer = new ResizeObserver(() => { setWidth(element.clientWidth) })
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [])

  const factor = zoom === 'auto' ? Math.max(0.1, Math.min(MAX_AUTO_ZOOM, (width - 48) / (size.width * PDF_TO_CSS))) : zoom
  const scale = factor * PDF_TO_CSS
  const pages = useMemo(() => Array.from({ length: pdf?.numPages ?? 0 }, (_unused, index) => index + 1), [pdf])

  const onScroll = (): void => {
    const element = stage.current
    if (element === null || pages.length === 0) return
    const pageHeight = size.height * scale + 12
    setPage(Math.min(pages.length, Math.max(1, Math.floor((element.scrollTop + element.clientHeight / 3) / pageHeight) + 1)))
  }
  const extract = async (): Promise<void> => {
    if (pdf === undefined) return
    setExtracting(true)
    try {
      const texts: string[] = []
      for (const number of pages) {
        const content = await (await pdf.getPage(number)).getTextContent()
        texts.push(`--- ${number} ---\n${content.items.map(item => item.str ?? '').join(' ')}`)
      }
      const text = texts.join('\n\n')
      if (await writeClipboard(text)) toast(t('pdfTextCopied', { count: text.length }), { tone: 'success' })
      else toast(t('copyFailed'))
    } catch (failure) {
      toast(t('pdfFailed', { error: failure instanceof Error ? failure.message : String(failure) }))
    } finally { setExtracting(false) }
  }

  return (
    <div className={css.viewer} data-viewer="pdf">
      <div className={css.toolbar}>
        <button type="button" className={css.toolButton} aria-label={t('zoomOut')} title={t('zoomOut')} onClick={() => { setZoom(Math.max(0.25, Math.round((factor - 0.25) * 4) / 4)) }}>−</button>
        <span>{t('zoomLevel', { percent: Math.round(factor * 100) })}</span>
        <button type="button" className={css.toolButton} aria-label={t('zoomIn')} title={t('zoomIn')} onClick={() => { setZoom(Math.min(5, Math.round((factor + 0.25) * 4) / 4)) }}>+</button>
        <button type="button" className={css.toolButton} aria-pressed={zoom === 'auto'} onClick={() => { setZoom('auto') }}>{t('zoomAuto')}</button>
        <span className={css.separator} />
        {pdf !== undefined && <span>{t('pdfPage', { page, pages: pdf.numPages })}</span>}
        <span className={css.grow} />
        <button type="button" className={css.toolButton} title={t('pdfTextTitle')} disabled={pdf === undefined || extracting} onClick={() => { void extract() }}>{t('pdfText')}</button>
      </div>
      <div ref={stage} className={css.stage} onScroll={onScroll}>
        {error !== '' ? <div className={`${css.message} ${css.error}`}>{t('pdfFailed', { error })}</div>
          : pdf === undefined ? <div className={css.message}>{t('loading')}</div>
            : <div className={css.pages}>{pages.map(number => (
              <PdfPageView key={number} document={pdf} number={number} size={size} scale={scale} root={stage.current} />
            ))}</div>}
      </div>
      <div className={css.status}><span>{fileSizeText(bytes)}</span></div>
    </div>
  )
}
