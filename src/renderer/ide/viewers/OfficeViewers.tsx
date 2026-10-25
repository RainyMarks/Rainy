/** Office documents and small Windows files: Word (.docx) rendered in the page, spreadsheets as tables, slides and legacy
 *  Word documents as PDF made by the Office installed on the computer, `~$` lock files and `.lnk` shortcuts. */
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { encryptedOpenXml, type OfficeFailureCode } from '../../../shared/office-protocol.ts'
import { fileSizeText, toast, writeClipboard } from '../../ui/index.ts'
import { HttpStatusError } from './byte-source.ts'
import { officeLockOwner, shortcutFacts } from './file-facts.ts'
import { loadFailure, type LoadFailure } from './load-errors.ts'
import { useViewerT } from './messages.ts'
import { PdfViewer } from './PdfViewer.tsx'
import { ToolActions, useNarrow, type ToolAction } from './ToolActions.tsx'
import { useZoomKeys } from './zoom-keys.ts'
import css from './Viewers.module.css'

/** Largest file read whole into the page for a preview. */
const MAX_PREVIEW_BYTES = 200 * 1024 * 1024
/** Spreadsheet cells drawn at once; Excel files can be far larger than a useful preview. */
const MAX_SHEET_ROWS = 2000
const MAX_SHEET_COLUMNS = 200

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/** A failure as one line in the reader's language; the original text is its tooltip. */
function Failed({ template, failure, block = false }: { template: (reason: string) => string; failure: LoadFailure; block?: boolean }): JSX.Element {
  const text = template(failure.reason)
  return block ? <div className={`${css.message} ${css.error}`} title={failure.detail}>{text}</div> : <p className={css.error} title={failure.detail}>{text}</p>
}

/** The whole file from the Host's byte route. */
function useFileBytes(url: string, size: number): { bytes?: Uint8Array; error?: LoadFailure } {
  const t = useViewerT()
  const [state, setState] = useState<{ bytes?: Uint8Array; error?: LoadFailure }>({})
  useEffect(() => {
    let cancelled = false
    setState({})
    if (size > MAX_PREVIEW_BYTES) { setState({ error: { reason: t('previewTooLarge', { size: fileSizeText(size) }), detail: '' } }); return }
    void fetch(url, { cache: 'no-store' }).then(async (response) => {
      if (!response.ok) throw new HttpStatusError(response.status)
      const bytes = new Uint8Array(await response.arrayBuffer())
      if (!cancelled) setState({ bytes })
    }).catch((error: unknown) => { if (!cancelled) setState({ error: loadFailure(error, t) }) })
    return () => { cancelled = true }
  }, [url, size, t])
  return state
}

/** Where a document opened from the tab lives, for "Open in default app". */
export interface DocumentLocation {
  /** Host path of the file; absent when the desktop cannot open it (SSH, or a plain browser). */
  readonly hostPath?: string | undefined
  readonly name: string
  readonly extension: string
}

/** "Open in default app" as a toolbar action, where the desktop can reach the file. */
function useOpenInApp(location: DocumentLocation): ToolAction | undefined {
  const t = useViewerT()
  const open = window.__RAINY_OFFICE__?.open
  if (open === undefined || location.hostPath === undefined) return undefined
  const path = location.hostPath
  return { id: 'open-in-app', label: t('openInApp'), title: t('openInAppTitle'), onSelect: () => {
    void open(path).then((result) => { if (result.error !== undefined) toast(t('openInAppFailed', { error: result.error }), { tone: 'error' }) },
      (error: unknown) => { toast(t('openInAppFailed', { error: message(error) }), { tone: 'error' }) })
  } }
}

function OpenInApp({ location }: { location: DocumentLocation }): JSX.Element | null {
  const action = useOpenInApp(location)
  if (action === undefined) return null
  return <button type="button" className={css.toolButton} title={action.title} onClick={action.onSelect}>{action.label}</button>
}

/** What a password-protected file gets instead of a preview. */
function Encrypted({ location }: { location: DocumentLocation }): JSX.Element {
  const t = useViewerT()
  return <div className={css.stage}><div className={css.factPage}>
    <p>{t('officeEncrypted')}</p>
    <div className={css.factActions}><OpenInApp location={location} /></div>
  </div></div>
}

/** The Word preview's zoom steps: tenths from 25% to 400%. */
function zoomStep(factor: number, direction: 1 | -1): number {
  return Math.min(4, Math.max(0.25, Math.round((factor + direction * 0.1) * 10) / 10))
}

/** Zoom buttons of the Word preview; on a narrow page 自动缩放 moves into the 更多 menu. */
function ZoomTools({ factor, auto, narrow, onZoom }: { factor: number; auto: boolean; narrow: boolean; onZoom: (zoom: number | 'auto') => void }): JSX.Element {
  const t = useViewerT()
  return <>
    <button type="button" className={css.toolButton} aria-label={t('zoomOut')} title={t('zoomOut')} onClick={() => { onZoom(zoomStep(factor, -1)) }}>−</button>
    <span>{t('zoomLevel', { percent: Math.round(factor * 100) })}</span>
    <button type="button" className={css.toolButton} aria-label={t('zoomIn')} title={t('zoomIn')} onClick={() => { onZoom(zoomStep(factor, 1)) }}>+</button>
    {!narrow && <button type="button" className={css.toolButton} aria-pressed={auto} onClick={() => { onZoom('auto') }}>{t('zoomAuto')}</button>}
  </>
}

// Pages keep their own white paper; only the wrapper around them follows the theme.
const WORD_STYLE = `
:host { display: block; }
.docx-wrapper { background: transparent !important; padding: 16px 0 !important; display: flex; flex-direction: column; align-items: center; }
.docx-wrapper > section.docx { box-shadow: 0 1px 4px rgb(0 0 0 / 20%); margin-bottom: 16px !important; }
`

/** A Word document drawn by docx-preview inside a shadow root, so its styles and the workbench's stay apart.
 *  `modes` lead the toolbar; `actions` fold into the 更多 menu on a narrow page. */
function WordPreview({ bytes, onFailed, modes, actions }: {
  bytes: Uint8Array
  onFailed: (error: unknown) => void
  modes: ReactNode
  actions: readonly (ToolAction | undefined)[]
}): JSX.Element {
  const t = useViewerT()
  const narrow = useNarrow()
  const [stage, setStage] = useState<HTMLDivElement | null>(null)
  const host = useRef<HTMLDivElement>(null)
  const [rendered, setRendered] = useState(false)
  const [pageWidth, setPageWidth] = useState(0)
  const [width, setWidth] = useState(800)
  const [zoom, setZoom] = useState<number | 'auto'>('auto')

  useEffect(() => {
    const element = host.current
    if (element === null) return
    let cancelled = false
    const root = element.shadowRoot ?? element.attachShadow({ mode: 'open' })
    root.replaceChildren()
    const style = document.createElement('style')
    style.textContent = WORD_STYLE
    const styles = document.createElement('div')
    const body = document.createElement('div')
    root.append(style, styles, body)
    setRendered(false)
    void import('docx-preview').then(async ({ renderAsync }) => {
      await renderAsync(bytes, body, styles, {
        className: 'docx', inWrapper: true, breakPages: true, ignoreLastRenderedPageBreak: true, renderHeaders: true, renderFooters: true,
        renderFootnotes: true, renderEndnotes: true, renderComments: false, renderChanges: false, renderAltChunks: false, useBase64URL: false, experimental: true,
      })
      if (cancelled) return
      // Links leave the page only for the web; other targets (files, scripts) are dropped.
      for (const link of body.querySelectorAll('a[href]')) {
        const href = link.getAttribute('href') ?? ''
        if (/^https?:\/\//iu.test(href)) { link.setAttribute('target', '_blank'); link.setAttribute('rel', 'noopener noreferrer') }
        else if (!href.startsWith('#')) link.removeAttribute('href')
      }
      const page = body.querySelector<HTMLElement>('section.docx')
      setPageWidth(page?.offsetWidth ?? 0)
      setRendered(true)
    }).catch((error: unknown) => { if (!cancelled) onFailed(error) })
    return () => {
      cancelled = true
      // docx-preview gives every image and font an object URL and never frees them.
      for (const url of new Set(root.innerHTML.match(/blob:[^"')\s]+/gu) ?? [])) URL.revokeObjectURL(url)
    }
  }, [bytes, onFailed])

  useLayoutEffect(() => {
    const element = stage
    if (element === null) return
    const observer = new ResizeObserver(() => { setWidth(element.clientWidth) })
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [stage])

  const fitted = pageWidth > 0 ? Math.max(0.25, Math.min(1.25, (width - 40) / pageWidth)) : 1
  const factor = zoom === 'auto' ? fitted : zoom
  // CSS zoom scales the pages from the stage's top left, so the scroll offsets scale with it to keep the reader's place.
  const zoomTo = (next: number | 'auto'): void => {
    const ratio = (next === 'auto' ? fitted : next) / factor
    if (stage !== null && ratio !== 1) {
      const top = stage.scrollTop * ratio
      const left = stage.scrollLeft * ratio
      requestAnimationFrame(() => { stage.scrollTop = top; stage.scrollLeft = left })
    }
    setZoom(next)
  }
  useZoomKeys(stage, { zoomIn: () => { zoomTo(zoomStep(factor, 1)) }, zoomOut: () => { zoomTo(zoomStep(factor, -1)) }, reset: () => { zoomTo('auto') } }, rendered)
  return <>
    <div className={css.toolbar}>
      {modes}
      <ZoomTools factor={factor} auto={zoom === 'auto'} narrow={narrow} onZoom={zoomTo} />
      <span className={css.grow} />
      <ToolActions actions={[narrow && { id: 'word-zoom-auto', label: t('zoomAuto'), disabled: zoom === 'auto', onSelect: () => { zoomTo('auto') } }, ...actions]} />
    </div>
    <div ref={setStage} className={`${css.stage} ${css.paperStage}`}>
      {!rendered && <div className={css.message}>{t('loading')}</div>}
      <div ref={host} style={{ zoom: factor }} hidden={!rendered} />
    </div>
  </>
}

/** Slides and legacy documents as a PDF made by the installed Office. */
interface OfficePdfState {
  pdf?: Uint8Array
  application?: string
  code?: OfficeFailureCode | undefined
  error?: string
  missing?: boolean
  encrypted?: boolean
  unavailable?: boolean
}

function useOfficePdf(bytes: Uint8Array | undefined, extension: string, attempt: number): OfficePdfState {
  const [state, setState] = useState<OfficePdfState>({})
  useEffect(() => {
    let cancelled = false
    setState({})
    if (bytes === undefined) return
    const bridge = window.__RAINY_OFFICE__
    if (bridge === undefined) { setState({ unavailable: true }); return }
    void bridge.toPdf(bytes, extension).then((result) => {
      if (cancelled) return
      setState('pdf' in result ? { pdf: result.pdf, application: result.application } : { code: result.code, error: result.error, missing: result.missing, encrypted: result.encrypted === true })
    }, (error: unknown) => { if (!cancelled) setState({ code: 'failed', error: message(error) }) })
    return () => { cancelled = true }
  }, [bytes, extension, attempt])
  return state
}

/** The text of each slide of a .pptx, for computers without Office. */
async function slideTexts(bytes: Uint8Array): Promise<string[][]> {
  const { default: JSZip } = await import('jszip')
  const zip = await JSZip.loadAsync(bytes)
  const slides = Object.keys(zip.files).filter(name => /^ppt\/slides\/slide\d+\.xml$/u.test(name))
    .sort((left, right) => Number(/\d+/u.exec(left.slice(11))?.[0]) - Number(/\d+/u.exec(right.slice(11))?.[0]))
  const parser = new DOMParser()
  return Promise.all(slides.map(async (name) => {
    const xml = parser.parseFromString(await zip.file(name)!.async('text'), 'application/xml')
    return [...xml.getElementsByTagNameNS('*', 'p')].filter(paragraph => paragraph.namespaceURI?.includes('drawingml') === true)
      .map(paragraph => [...paragraph.getElementsByTagNameNS('*', 't')].map(text => text.textContent ?? '').join('').trim())
      .filter(line => line !== '')
  }))
}

function SlidesOutline({ bytes }: { bytes: Uint8Array }): JSX.Element {
  const t = useViewerT()
  const [slides, setSlides] = useState<string[][] | undefined>()
  const [error, setError] = useState<LoadFailure | undefined>()
  useEffect(() => {
    let cancelled = false
    void slideTexts(bytes).then((value) => { if (!cancelled) setSlides(value) }, (failure: unknown) => { if (!cancelled) setError(loadFailure(failure, t)) })
    return () => { cancelled = true }
  }, [bytes, t])
  if (error !== undefined) return <Failed template={reason => t('slidesOutlineFailed', { error: reason })} failure={error} />
  if (slides === undefined) return <p>{t('loading')}</p>
  return <div className={css.outline}>
    <p>{t('slidesOutline')}</p>
    {slides.map((lines, index) => <section key={index} className={css.outlineSlide}>
      <h4>{t('slidesSlide', { number: index + 1 })}</h4>
      {lines.length === 0 ? <p className={css.muted}>{t('slidesNoText')}</p> : lines.map((line, row) => <p key={row}>{line}</p>)}
    </section>)}
  </div>
}

/** Why Office made no PDF, in the reader's language; Office's own words are only the detail. */
function officeFailure(result: OfficePdfState, t: ReturnType<typeof useViewerT>): string {
  switch (result.code) {
    case 'timeout': return t('officeTimeout')
    case 'too-large': return t('officeTooLarge')
    case 'no-output': return t('officeNoOutput')
    case 'unsupported': return t('officeUnsupported')
    case undefined: return result.error === undefined || result.error === '' ? t('officeFailedPlain') : t('officeFailed', { error: result.error })
    default: return t('officeFailedPlain')
  }
}

function OfficePdf({ bytes, location, attempt, onRetry, fallback, modes }: {
  bytes: Uint8Array
  location: DocumentLocation
  attempt: number
  onRetry: () => void
  fallback?: ReactNode
  modes?: ReactNode
}): JSX.Element {
  const t = useViewerT()
  const result = useOfficePdf(bytes, location.extension, attempt)
  const openInApp = useOpenInApp(location)
  if (result.encrypted === true) return <>{modes !== undefined && <div className={css.toolbar}>{modes}</div>}<Encrypted location={location} /></>
  if (result.pdf !== undefined) {
    return <PdfViewer data={result.pdf} bytes={bytes.length} note={t('officeConvertedBy', { app: result.application ?? 'Office' })}
      modes={modes} actions={openInApp === undefined ? [] : [openInApp]} />
  }
  const failed = result.unavailable === true || result.missing === true || result.error !== undefined
  // With a code the reason is ours, and what Office or Windows said is shown below it.
  const detail = result.code !== undefined && result.error !== undefined && result.error !== '' ? result.error : undefined
  return <>
    {modes !== undefined && <div className={css.toolbar}>{modes}</div>}
    <div className={css.stage}>
      {!failed ? <div className={css.message}>{t('officeConverting')}</div> : <div className={css.factPage}>
        <p className={result.error !== undefined && result.missing !== true ? css.error : undefined} title={detail}>
          {result.unavailable === true ? t('officeUnavailable') : result.missing === true ? t('officeMissing') : officeFailure(result, t)}
        </p>
        {result.missing !== true && detail !== undefined && <p className={css.muted}>{t('officeDetail', { detail })}</p>}
        <div className={css.factActions}>
          {result.error !== undefined && result.missing !== true && <button type="button" className={css.toolButton} onClick={onRetry}>{t('officeRetry')}</button>}
          <OpenInApp location={location} />
        </div>
        {fallback}
      </div>}
    </div>
  </>
}

/**
 * A .docx shown by Rainy itself, with the Office layout one click away.
 * @param props.url Byte route URL.
 * @param props.size File size.
 * @param props.location File name, extension and Host path.
 * @returns The viewer.
 */
export function WordViewer({ url, size, location }: { url: string; size: number; location: DocumentLocation }): JSX.Element {
  const t = useViewerT()
  const file = useFileBytes(url, size)
  const [office, setOffice] = useState(false)
  const [failure, setFailure] = useState<LoadFailure | undefined>()
  const [attempt, setAttempt] = useState(0)
  const openInApp = useOpenInApp(location)
  useEffect(() => { setFailure(undefined) }, [url])
  const onFailed = useMemo(() => (error: unknown) => { setFailure(loadFailure(error, t)) }, [t])
  const canConvert = window.__RAINY_OFFICE__ !== undefined
  // The two modes lead the toolbar in both of them, so switching back is always where it was.
  const modes = canConvert ? <>
    <button type="button" className={css.toolButton} aria-pressed={!office} title={t('docxBuiltInTitle')} onClick={() => { setOffice(false) }}>{t('docxBuiltIn')}</button>
    <button type="button" className={css.toolButton} aria-pressed={office} title={t('docxOfficeTitle')} onClick={() => { setOffice(true) }}>{t('docxOffice')}</button>
    <span className={css.separator} />
  </> : undefined
  let body: JSX.Element
  if (file.error !== undefined) body = <Failed block template={reason => t('loadFailed', { error: reason })} failure={file.error} />
  else if (file.bytes === undefined) body = <div className={css.message}>{t('loading')}</div>
  else if (encryptedOpenXml(file.bytes, location.extension)) body = <Encrypted location={location} />
  else if (office) body = <OfficePdf bytes={file.bytes} location={location} attempt={attempt} onRetry={() => { setAttempt(value => value + 1) }} modes={modes} />
  else if (failure !== undefined) {
    body = <div className={css.stage}><div className={css.factPage}>
      <Failed template={reason => t('docxFailed', { error: reason })} failure={failure} />
      <div className={css.factActions}>
        {canConvert && <button type="button" className={css.toolButton} onClick={() => { setOffice(true) }}>{t('docxOffice')}</button>}
        <OpenInApp location={location} />
      </div>
    </div></div>
  } else {
    body = <WordPreview bytes={file.bytes} onFailed={onFailed} modes={modes} actions={[openInApp]} />
  }
  return <div className={css.viewer} data-viewer="word">
    {body}
    {!office && <div className={css.status}><span>{fileSizeText(size)}</span></div>}
  </div>
}

/**
 * Slides (.pptx, .ppt) and legacy Word documents through the installed Office; the slide text when there is none.
 * @param props.url Byte route URL.
 * @param props.size File size.
 * @param props.location File name, extension and Host path.
 * @returns The viewer.
 */
export function OfficeViewer({ url, size, location }: { url: string; size: number; location: DocumentLocation }): JSX.Element {
  const t = useViewerT()
  const file = useFileBytes(url, size)
  const [attempt, setAttempt] = useState(0)
  const slides = ['pptx', 'pptm', 'ppsx', 'ppsm', 'potx'].includes(location.extension)
  return <div className={css.viewer} data-viewer="office">
    {file.error !== undefined ? <Failed block template={reason => t('loadFailed', { error: reason })} failure={file.error} />
      : file.bytes === undefined ? <div className={css.message}>{t('loading')}</div>
        : encryptedOpenXml(file.bytes, location.extension) ? <Encrypted location={location} />
        : <OfficePdf bytes={file.bytes} location={location} attempt={attempt} onRetry={() => { setAttempt(value => value + 1) }}
          fallback={slides ? <SlidesOutline bytes={file.bytes} /> : undefined} />}
  </div>
}

/** One sheet as rows of display text, with merged cells. */
interface SheetGrid {
  readonly name: string
  readonly rows: readonly (readonly string[])[]
  readonly totalRows: number
  readonly totalColumns: number
  /** `row:column` of each merge's top-left cell → its span; covered cells are listed in `hidden`. */
  readonly spans: ReadonlyMap<string, { rows: number; columns: number }>
  readonly hidden: ReadonlySet<string>
}

/** Excel's built-in short date formats follow the system language, as in Excel; the others are shown as written. */
const SHORT_DATES: Readonly<Record<string, { zh: string; en: string }>> = {
  'm/d/yy': { zh: 'yyyy/m/d', en: 'm/d/yyyy' },
  'm/d/yy h:mm': { zh: 'yyyy/m/d h:mm', en: 'm/d/yyyy h:mm' },
}

async function readSheets(bytes: Uint8Array, locale: 'zh' | 'en'): Promise<SheetGrid[]> {
  const XLSX = await import('xlsx')
  const book = XLSX.read(bytes, { type: 'array', cellNF: true, cellStyles: false, sheetRows: MAX_SHEET_ROWS + 1 })
  return book.SheetNames.map((name) => {
    const sheet = book.Sheets[name]
    if (sheet === undefined || sheet['!ref'] === undefined) return { name, rows: [], totalRows: 0, totalColumns: 0, spans: new Map(), hidden: new Set() }
    for (const [address, cell] of Object.entries(sheet)) {
      if (address.startsWith('!') || typeof cell !== 'object' || cell === null || !('t' in cell)) continue
      const short = typeof cell.z === 'string' ? SHORT_DATES[cell.z] : undefined
      if (short !== undefined && cell.t === 'n' && typeof cell.v === 'number') cell.w = XLSX.SSF.format(short[locale], cell.v)
    }
    const range = XLSX.utils.decode_range(sheet['!fullref'] ?? sheet['!ref'])
    const totalRows = range.e.r + 1
    const totalColumns = range.e.c + 1
    const rows = XLSX.utils.sheet_to_json<string[]>(sheet, { header: 1, raw: false, defval: '', blankrows: true, dateNF: 'yyyy-mm-dd' })
      .slice(0, MAX_SHEET_ROWS).map(row => row.slice(0, MAX_SHEET_COLUMNS).map(String))
    const spans = new Map<string, { rows: number; columns: number }>()
    const hidden = new Set<string>()
    for (const merge of sheet['!merges'] ?? []) {
      spans.set(`${merge.s.r}:${merge.s.c}`, { rows: merge.e.r - merge.s.r + 1, columns: merge.e.c - merge.s.c + 1 })
      for (let row = merge.s.r; row <= merge.e.r; row++) {
        for (let column = merge.s.c; column <= merge.e.c; column++) if (row !== merge.s.r || column !== merge.s.c) hidden.add(`${row}:${column}`)
      }
    }
    return { name, rows, totalRows, totalColumns, spans, hidden }
  })
}

/** Marks a spreadsheet that needs a password, in place of an error text. */
const PROTECTED = '\0protected'

function columnName(index: number): string {
  let name = ''
  for (let value = index + 1; value > 0; value = Math.floor((value - 1) / 26)) name = String.fromCharCode(65 + ((value - 1) % 26)) + name
  return name
}

/**
 * Excel and OpenDocument spreadsheets as tables, one tab per sheet.
 * @param props.url Byte route URL.
 * @param props.size File size.
 * @param props.location File name, extension and Host path.
 * @returns The viewer.
 */
export function SheetViewer({ url, size, location }: { url: string; size: number; location: DocumentLocation }): JSX.Element {
  const t = useViewerT()
  const file = useFileBytes(url, size)
  const openInApp = useOpenInApp(location)
  const [sheets, setSheets] = useState<SheetGrid[] | undefined>()
  const [error, setError] = useState<LoadFailure | typeof PROTECTED | undefined>()
  const [active, setActive] = useState(0)
  useEffect(() => {
    let cancelled = false
    setSheets(undefined); setError(undefined); setActive(0)
    if (file.bytes === undefined) return
    if (encryptedOpenXml(file.bytes, location.extension)) { setError(PROTECTED); return }
    void readSheets(file.bytes, document.documentElement.lang.startsWith('zh') ? 'zh' : 'en').then((value) => { if (!cancelled) setSheets(value) },
      (failure: unknown) => { if (!cancelled) setError(/password/iu.test(message(failure)) ? PROTECTED : loadFailure(failure, t)) })
    return () => { cancelled = true }
  }, [file.bytes, location.extension, t])
  const sheet = sheets?.[active]
  const columns = useMemo(() => Math.min(MAX_SHEET_COLUMNS, Math.max(sheet?.totalColumns ?? 0, ...(sheet?.rows ?? []).map(row => row.length))), [sheet])
  const failure = file.error ?? (error === PROTECTED ? undefined : error)
  if (error === PROTECTED) return <div className={css.viewer} data-viewer="sheet"><Encrypted location={location} /></div>
  return <div className={css.viewer} data-viewer="sheet">
    {openInApp !== undefined && <div className={css.toolbar}><span className={css.grow} /><ToolActions actions={[openInApp]} /></div>}
    <div className={css.stage}>
      {failure !== undefined ? <Failed block template={reason => t('sheetFailed', { error: reason })} failure={failure} />
        : sheet === undefined ? <div className={css.message}>{t('loading')}</div>
          : sheet.rows.length === 0 ? <div className={css.message}>{t('sheetEmpty')}</div>
            : <table className={css.sheet}>
              <thead><tr><th />{Array.from({ length: columns }, (_unused, column) => <th key={column}>{columnName(column)}</th>)}</tr></thead>
              <tbody>{sheet.rows.map((row, rowIndex) => <tr key={rowIndex}>
                <th>{rowIndex + 1}</th>
                {Array.from({ length: columns }, (_unused, column) => {
                  const key = `${rowIndex}:${column}`
                  if (sheet.hidden.has(key)) return null
                  const span = sheet.spans.get(key)
                  const value = row[column] ?? ''
                  return <td key={column} rowSpan={span?.rows} colSpan={span?.columns} data-number={/^-?[\d,.]+%?$/u.test(value) ? '' : undefined}>{value}</td>
                })}
              </tr>)}</tbody>
            </table>}
    </div>
    {sheets !== undefined && sheets.length > 1 && <div className={css.sheetTabs} role="tablist" aria-label={t('sheetTabs')}>
      {sheets.map((value, index) => <button key={value.name} type="button" role="tab" aria-selected={index === active} className={css.sheetTab} onClick={() => { setActive(index) }}>{value.name}</button>)}
    </div>}
    <div className={css.status}>
      {sheet !== undefined && <span>{t('sheetRows', { rows: sheet.totalRows, columns: sheet.totalColumns })}</span>}
      {sheet !== undefined && (sheet.totalRows > MAX_SHEET_ROWS || sheet.totalColumns > MAX_SHEET_COLUMNS)
        && <span>{t('sheetTruncated', { rows: MAX_SHEET_ROWS, columns: MAX_SHEET_COLUMNS })}</span>}
      <span>{fileSizeText(size)}</span>
    </div>
  </div>
}

/**
 * What an Office `~$` owner file says, instead of its bytes.
 * @param props.url Byte route URL.
 * @param props.size File size.
 * @param props.document Name of the document the lock belongs to, or the part of it the lock's own name keeps.
 * @param props.onOpenDocument Opens that document, when it is in the same folder.
 * @returns The viewer.
 */
export function OfficeLockViewer({ url, size, document, onOpenDocument }: { url: string; size: number; document: string; onOpenDocument?: (() => void) | undefined }): JSX.Element {
  const t = useViewerT()
  const file = useFileBytes(url, size)
  const owner = file.bytes === undefined ? undefined : officeLockOwner(file.bytes)
  return <div className={css.viewer} data-viewer="office-lock">
    <div className={css.stage}><div className={css.factPage}>
      <h3>{t('lockTitle')}</h3>
      {file.bytes !== undefined && <p>{owner === undefined ? t('lockOwnerUnknown', { name: document }) : t('lockOwner', { owner, name: document })}</p>}
      <p className={css.muted}>{t('lockExplain')}</p>
      {onOpenDocument !== undefined && <div className={css.factActions}>
        <button type="button" className={css.toolButton} onClick={onOpenDocument}>{t('lockOpenDocument', { name: document })}</button>
      </div>}
    </div></div>
    <div className={css.status}><span>{fileSizeText(size)}</span></div>
  </div>
}

/**
 * Where a Windows shortcut points.
 * @param props.url Byte route URL.
 * @param props.size File size.
 * @returns The viewer.
 */
export function ShortcutViewer({ url, size }: { url: string; size: number }): JSX.Element {
  const t = useViewerT()
  const file = useFileBytes(url, size)
  const facts = file.bytes === undefined ? undefined : shortcutFacts(file.bytes)
  const rows: [string, string | undefined][] = facts === undefined ? [] : [
    [t('shortcutTarget'), facts.target], [t('shortcutArguments'), facts.arguments], [t('shortcutFolder'), facts.workingDirectory], [t('shortcutComment'), facts.description],
  ]
  return <div className={css.viewer} data-viewer="shortcut">
    <div className={css.stage}><div className={css.factPage}>
      <h3>{t('shortcutTitle')}</h3>
      {file.error !== undefined ? <Failed template={reason => t('loadFailed', { error: reason })} failure={file.error} />
        : file.bytes === undefined ? <p>{t('loading')}</p>
          : facts === undefined ? <p className={css.error}>{t('shortcutBroken')}</p>
            : <table className={css.facts}><tbody>{rows.filter((row): row is [string, string] => row[1] !== undefined).map(([label, value]) => (
              <tr key={label}><td>{label}</td><td>{value}</td></tr>
            ))}</tbody></table>}
      {facts?.target !== undefined && <div className={css.factActions}>
        <button type="button" className={css.toolButton} onClick={() => {
          void writeClipboard(facts.target!, { quiet: true }).then((copied) => { toast(copied ? t('shortcutCopied') : t('copyFailed'), { tone: copied ? 'success' : 'error' }) })
        }}>{t('shortcutCopy')}</button>
      </div>}
    </div></div>
    <div className={css.status}><span>{fileSizeText(size)}</span></div>
  </div>
}
