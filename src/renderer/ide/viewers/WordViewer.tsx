/** Word (.docx) viewer: detached render + DOM/CSS sanitization before ShadowRoot mount, auto-fit zoom, and Office PDF mode. */
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { encryptedOpenXml } from '../../../shared/office-protocol.ts'
import { fileSizeText } from '../../ui/index.ts'
import { revokeBlobUrls, sanitizeDocxDom } from './docx-sanitize.ts'
import { loadFailure, type LoadFailure } from './load-errors.ts'
import { useViewerT } from './messages.ts'
import { Encrypted, Failed, OfficePdf, OpenInApp, useFileBytes, useOpenInApp, type DocumentLocation } from './office-shared.tsx'
import { ToolActions, useNarrow, type ToolAction } from './ToolActions.tsx'
import { useZoomKeys } from './zoom-keys.ts'
import css from './Viewers.module.css'

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

/** A Word document drawn by docx-preview inside a shadow root, sanitized while detached before mounting. */
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
  const zoomFrame = useRef(0)
  const [rendered, setRendered] = useState(false)
  const [pageWidth, setPageWidth] = useState(0)
  const [width, setWidth] = useState(800)
  const [zoom, setZoom] = useState<number | 'auto'>('auto')

  useEffect(() => {
    const element = host.current
    if (element === null) return
    let cancelled = false
    const revoked = new Set<string>()
    const root = element.shadowRoot ?? element.attachShadow({ mode: 'open' })
    revokeBlobUrls(root, revoked)
    root.replaceChildren()
    // Render and sanitize into detached containers so untrusted docx markup/styles never touch the live DOM before sanitization.
    const styles = document.createElement('div')
    const body = document.createElement('div')
    const cleanupBlobs = (): void => {
      revokeBlobUrls(root, revoked)
      revokeBlobUrls(styles, revoked)
      revokeBlobUrls(body, revoked)
      root.replaceChildren()
    }
    setRendered(false)
    setPageWidth(0)
    void import('docx-preview').then(async ({ renderAsync }) => {
      if (cancelled) return
      await renderAsync(bytes, body, styles, {
        className: 'docx', inWrapper: true, breakPages: true, ignoreLastRenderedPageBreak: true, renderHeaders: true, renderFooters: true,
        renderFootnotes: true, renderEndnotes: true, renderComments: false, renderChanges: false, renderAltChunks: false, useBase64URL: false, experimental: true,
      })
      if (cancelled) {
        // Release any blob: URLs docx-preview created into detached `styles`/`body` after unmount.
        cleanupBlobs()
        return
      }
      sanitizeDocxDom(styles, body)
      const style = document.createElement('style')
      style.textContent = WORD_STYLE
      root.append(style, styles, body)
      setRendered(true)
    }).catch((error: unknown) => {
      cleanupBlobs()
      if (!cancelled) onFailed(error)
    })
    return () => {
      cancelled = true
      cancelAnimationFrame(zoomFrame.current)
      // docx-preview gives every image and font an object URL and never frees them.
      cleanupBlobs()
    }
  }, [bytes, onFailed])

  // Measure page width only AFTER `hidden={!rendered}` is lifted so offsetWidth is real instead of 0.
  useLayoutEffect(() => {
    if (!rendered) return
    const page = host.current?.shadowRoot?.querySelector<HTMLElement>('section.docx')
    if (page === null || page === undefined) return
    const measure = (): void => {
      const measured = page.offsetWidth
      if (measured > 0) setPageWidth(measured)
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(page)
    return () => { observer.disconnect() }
  }, [rendered])

  useLayoutEffect(() => {
    const element = stage
    if (element === null) return
    const measure = (): void => {
      if (element.clientWidth > 0) setWidth(element.clientWidth)
    }
    measure()
    const observer = new ResizeObserver(measure)
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
      cancelAnimationFrame(zoomFrame.current)
      zoomFrame.current = requestAnimationFrame(() => { stage.scrollTop = top; stage.scrollLeft = left })
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
  useEffect(() => { setFailure(undefined); setOffice(false); setAttempt(0) }, [url])
  const latestT = useRef(t)
  latestT.current = t
  const onFailed = useMemo(() => (error: unknown) => { setFailure(loadFailure(error, latestT.current)) }, [])
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
  else if (office) body = <OfficePdf bytes={file.bytes} size={size} location={location} attempt={attempt} onRetry={() => { setAttempt(value => value + 1) }} modes={modes} />
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
