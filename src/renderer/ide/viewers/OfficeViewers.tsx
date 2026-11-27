/** Office slides/legacy viewer and small Windows bookkeeping files (`~$` lock files and `.lnk` shortcuts),
 *  plus re-exports of WordViewer and SheetViewer for existing callers. */
import { useEffect, useRef, useState } from 'react'
import { encryptedOpenXml } from '../../../shared/office-protocol.ts'
import { fileSizeText, toast, writeClipboard } from '../../ui/index.ts'
import { officeLockOwner, shortcutFacts } from './file-facts.ts'
import { loadFailure, type LoadFailure } from './load-errors.ts'
import { useViewerT } from './messages.ts'
import { Encrypted, Failed, MAX_PREVIEW_BYTES, OfficePdf, pdfSourceOf, useFileBytes, type DocumentLocation } from './office-shared.tsx'
import css from './Viewers.module.css'

export { type DocumentLocation } from './office-shared.tsx'
export { WordViewer } from './WordViewer.tsx'
export { SheetViewer } from './SheetViewer.tsx'

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
  const latestT = useRef(t)
  latestT.current = t
  const [slides, setSlides] = useState<string[][] | undefined>()
  const [error, setError] = useState<LoadFailure | undefined>()
  useEffect(() => {
    let cancelled = false
    setSlides(undefined); setError(undefined)
    void slideTexts(bytes).then((value) => { if (!cancelled) setSlides(value) }, (failure: unknown) => { if (!cancelled) setError(loadFailure(failure, latestT.current)) })
    return () => { cancelled = true }
  }, [bytes])
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

function LazySlidesOutline({ url, size, bytes }: { url?: string | undefined; size: number; bytes?: Uint8Array | undefined }): JSX.Element {
  const t = useViewerT()
  const fetched = useFileBytes(bytes === undefined && url !== undefined ? url : '', bytes === undefined && url !== undefined ? size : 0)
  const data = bytes ?? fetched.bytes
  if (bytes === undefined && fetched.error !== undefined) return <Failed template={reason => t('loadFailed', { error: reason })} failure={fetched.error} />
  if (data === undefined) return <p>{t('loading')}</p>
  return <SlidesOutline bytes={data} />
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
  const direct = window.__RAINY_OFFICE__ !== undefined && pdfSourceOf(location) !== undefined
  // When the desktop bridge can stream directly from the Host via file coordinates, skip downloading up to 200 MB
  // into the renderer; fall back to fetching only when SlidesOutline is actually rendered or no direct bridge exists.
  const file = useFileBytes(direct ? '' : url, direct ? 0 : size)
  const [attempt, setAttempt] = useState(0)
  const slides = ['pptx', 'pptm', 'ppsx', 'ppsm', 'potx'].includes(location.extension)
  if (direct) {
    return <div className={css.viewer} data-viewer="office">
      {size > MAX_PREVIEW_BYTES
        ? <Failed block template={reason => t('loadFailed', { error: reason })} failure={{ reason: t('previewTooLarge', { size: fileSizeText(size) }), detail: '' }} />
        : <OfficePdf size={size} location={location} attempt={attempt} onRetry={() => { setAttempt(value => value + 1) }}
          fallback={slides ? <LazySlidesOutline url={url} size={size} /> : undefined} />}
    </div>
  }
  return <div className={css.viewer} data-viewer="office">
    {file.error !== undefined ? <Failed block template={reason => t('loadFailed', { error: reason })} failure={file.error} />
      : file.bytes === undefined ? <div className={css.message}>{t('loading')}</div>
        : encryptedOpenXml(file.bytes, location.extension) ? <Encrypted location={location} />
        : <OfficePdf bytes={file.bytes} size={size} location={location} attempt={attempt} onRetry={() => { setAttempt(value => value + 1) }}
          fallback={slides ? <SlidesOutline bytes={file.bytes} /> : undefined} />}
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
      {file.error !== undefined ? <Failed template={reason => t('loadFailed', { error: reason })} failure={file.error} />
        : file.bytes === undefined ? <p>{t('loading')}</p>
          : <p>{owner === undefined ? t('lockOwnerUnknown', { name: document }) : t('lockOwner', { owner, name: document })}</p>}
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
