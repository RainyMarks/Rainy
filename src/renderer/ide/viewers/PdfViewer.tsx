/** PDF viewer on the bundled pdf.js 2.16 build (`resources/pdfjs`): pages render on demand, a password-protected file asks
 *  for its password, and the text can be extracted. */
import { useEffect, useId, useMemo, useRef, useState, type ReactNode } from 'react'
import { Button, fileSizeText, isComposingKey, toast, writeClipboard } from '../../ui/index.ts'
import { useViewerT } from './messages.ts'
import { describePdfError, PdfJsUnavailableError } from './pdf-errors.ts'
import { joinTextItems, type PdfTextItem } from './pdf-text.ts'
import css from './Viewers.module.css'

/** The parts of the pdf.js 2.16 API this viewer uses. */
interface PdfPage {
  getViewport(options: { scale: number }): { width: number; height: number }
  render(options: { canvasContext: CanvasRenderingContext2D; viewport: unknown }): { promise: Promise<void>; cancel(): void }
  getTextContent(): Promise<{ items: readonly PdfTextItem[] }>
  cleanup(): void
}
interface PdfDocument { numPages: number; getPage(page: number): Promise<PdfPage> }
interface PdfLoadingTask {
  readonly promise: Promise<PdfDocument>
  /** Stops loading, or closes the document once it is open. */
  destroy(): Promise<void>
  /** Called instead of failing when the file needs a password, and again with `INCORRECT_PASSWORD` after a wrong one. */
  onPassword: ((update: (password: string) => void, reason: number) => void) | null
}
interface PdfJs {
  GlobalWorkerOptions: { workerSrc: string }
  getDocument(options: Record<string, unknown>): PdfLoadingTask
  readonly PasswordResponses: { readonly NEED_PASSWORD: number; readonly INCORRECT_PASSWORD: number }
}

const PDFJS_ROOT = '/rainy/pdfjs/'
/** pdf.js measures pages in points; 100% shows a page at its printed size on a 96 dpi screen. */
const PDF_TO_CSS = 96 / 72
/** Automatic zoom fits the page width but stops here, as in pdf.js. */
const MAX_AUTO_ZOOM = 1.25
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
 * @param props.url Byte route URL of the PDF.
 * @param props.data The PDF itself, for a copy made from another format; it takes the place of `url`.
 * @param props.bytes File size shown in the status bar.
 * @param props.tools Extra toolbar items, after the text button.
 * @param props.note Status bar text before the size.
 * @returns The viewer.
 */
export function PdfViewer({ url, data, bytes, tools, note }: {
  url?: string | undefined
  data?: Uint8Array | undefined
  bytes: number
  tools?: ReactNode
  note?: string | undefined
}): JSX.Element {
  const t = useViewerT()
  const stage = useRef<HTMLDivElement>(null)
  const [pdf, setPdf] = useState<PdfDocument | undefined>()
  const [size, setSize] = useState<{ width: number; height: number }>({ width: 612, height: 792 })
  const [zoom, setZoom] = useState<number | 'auto'>('auto')
  const [width, setWidth] = useState(800)
  const [page, setPage] = useState(1)
  const [failure, setFailure] = useState<{ error: unknown } | undefined>()
  const [password, setPassword] = useState<{ submit: (password: string) => void; incorrect: boolean; attempt: number } | undefined>()
  const [extracting, setExtracting] = useState(false)

  useEffect(() => {
    let cancelled = false
    let task: PdfLoadingTask | undefined
    setPdf(undefined); setFailure(undefined); setPassword(undefined); setPage(1)
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
      setPdf(document)
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
        {tools}
      </div>
      <div ref={stage} className={css.stage} onScroll={onScroll}>
        {problem !== undefined ? (
          <div className={css.message}>
            <span className={css.error} title={problem.detail}>{t('pdfFailed', { error: t(problem.key, problem.vars) })}</span>
            {problem.detailed && <span className={css.pdfErrorDetail}>{problem.detail}</span>}
          </div>
        ) : password !== undefined ? <PdfPassword key={password.attempt} incorrect={password.incorrect} onSubmit={password.submit} />
          : pdf === undefined ? <div className={css.message}>{t('loading')}</div>
            : <div className={css.pages}>{pages.map(number => (
              <PdfPageView key={number} document={pdf} number={number} size={size} scale={scale} root={stage.current} />
            ))}</div>}
      </div>
      <div className={css.status}>{note !== undefined && <span>{note}</span>}<span>{fileSizeText(bytes)}</span></div>
    </div>
  )
}
