/** Shared helpers for Office document viewers: bounded file fetch, desktop Office-to-PDF conversion, and error states. */
import { useEffect, useRef, useState, type ReactNode } from 'react'
import type { IdeRootId, WorkspaceId } from '../../../shared/ide-files-protocol.ts'
import type { OfficeFailureCode, OfficePdfSource } from '../../../shared/office-protocol.ts'
import { fileSizeText, toast } from '../../ui/index.ts'
import { HttpStatusError } from './byte-source.ts'
import { loadFailure, type LoadFailure } from './load-errors.ts'
import { useViewerT } from './messages.ts'
import { PdfViewer } from './PdfViewer.tsx'
import type { ToolAction } from './ToolActions.tsx'
import { errorMessage } from './viewer-utils.ts'
import css from './Viewers.module.css'

/** Largest file read whole into the page for a preview. */
export const MAX_PREVIEW_BYTES = 200 * 1024 * 1024

/** A failure as one line in the reader's language; the original text is its tooltip. */
export function Failed({ template, failure, block = false }: { template: (reason: string) => string; failure: LoadFailure; block?: boolean }): JSX.Element {
  const text = template(failure.reason)
  return block ? <div className={`${css.message} ${css.error}`} role="alert" title={failure.detail}>{text}</div> : <p className={css.error} role="alert" title={failure.detail}>{text}</p>
}

/**
 * Fetch the whole file from the Host's byte route with AbortController cancellation and actual payload size guard.
 * Keeps `t` in a ref so switching UI language never re-downloads up to 200 MB.
 */
export function useFileBytes(url: string, size: number): { bytes?: Uint8Array; error?: LoadFailure } {
  const t = useViewerT()
  const latestT = useRef(t)
  latestT.current = t
  const [state, setState] = useState<{ bytes?: Uint8Array; error?: LoadFailure }>({})
  useEffect(() => {
    let cancelled = false
    const controller = new AbortController()
    setState({})
    if (url === '') return undefined
    if (size > MAX_PREVIEW_BYTES) {
      setState({ error: { reason: latestT.current('previewTooLarge', { size: fileSizeText(size) }), detail: '' } })
      return undefined
    }
    void fetch(url, { cache: 'no-store', signal: controller.signal }).then(async (response) => {
      if (!response.ok) throw new HttpStatusError(response.status)
      const declared = Number(response.headers.get('content-length') ?? '0')
      if (Number.isFinite(declared) && declared > MAX_PREVIEW_BYTES) {
        if (!cancelled) setState({ error: { reason: latestT.current('previewTooLarge', { size: fileSizeText(declared) }), detail: '' } })
        return
      }
      const buffer = await response.arrayBuffer()
      if (cancelled) return
      if (buffer.byteLength > MAX_PREVIEW_BYTES) {
        setState({ error: { reason: latestT.current('previewTooLarge', { size: fileSizeText(buffer.byteLength) }), detail: '' } })
        return
      }
      setState({ bytes: new Uint8Array(buffer) })
    }).catch((error: unknown) => {
      if (cancelled || (error instanceof DOMException && error.name === 'AbortError')) return
      setState({ error: loadFailure(error, latestT.current) })
    })
    return () => { cancelled = true; controller.abort() }
  }, [url, size])
  return state
}

/** Where a document opened from the tab lives, for "Open in default app" and zero-Base64 Office conversion. */
export interface DocumentLocation {
  /** Host path of the file; absent when the desktop cannot open it (SSH, or a plain browser). */
  readonly hostPath?: string | undefined
  readonly name: string
  readonly extension: string
  /** Workspace file coordinates; when present, Office → PDF conversion streams directly through the Host without Base64. */
  readonly workspaceId?: WorkspaceId | undefined
  readonly rootId?: IdeRootId | undefined
  readonly path?: string | undefined
  readonly version?: string | undefined
}

export function pdfSourceOf(location: DocumentLocation): OfficePdfSource | undefined {
  if (location.workspaceId === undefined || location.path === undefined || location.path === '') return undefined
  return {
    workspaceId: location.workspaceId,
    rootId: location.rootId,
    path: location.path,
    version: location.version,
    extension: location.extension,
  }
}

/** "Open in default app" as a toolbar action, where the desktop can reach the file. */
export function useOpenInApp(location: DocumentLocation): ToolAction | undefined {
  const t = useViewerT()
  const open = window.__RAINY_OFFICE__?.open
  if (open === undefined || location.hostPath === undefined) return undefined
  const path = location.hostPath
  return { id: 'open-in-app', label: t('openInApp'), title: t('openInAppTitle'), onSelect: () => {
    void open(path).then((result) => { if (result.error !== undefined) toast(t('openInAppFailed', { error: result.error }), { tone: 'error' }) },
      (error: unknown) => { toast(t('openInAppFailed', { error: errorMessage(error) }), { tone: 'error' }) })
  } }
}

export function OpenInApp({ location }: { location: DocumentLocation }): JSX.Element | null {
  const action = useOpenInApp(location)
  if (action === undefined) return null
  return <button type="button" className={css.toolButton} title={action.title} onClick={action.onSelect}>{action.label}</button>
}

/** What a password-protected file gets instead of a preview. */
export function Encrypted({ location }: { location: DocumentLocation }): JSX.Element {
  const t = useViewerT()
  return <div className={css.stage}><div className={css.factPage}>
    <p>{t('officeEncrypted')}</p>
    <div className={css.factActions}><OpenInApp location={location} /></div>
  </div></div>
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

function useOfficePdf(source: OfficePdfSource | undefined, bytes: Uint8Array | undefined, extension: string, attempt: number): OfficePdfState {
  const [state, setState] = useState<OfficePdfState>({})
  useEffect(() => {
    let cancelled = false
    setState({})
    if (source === undefined && bytes === undefined) return
    const bridge = window.__RAINY_OFFICE__
    if (bridge === undefined) { setState({ unavailable: true }); return }
    const pending = source !== undefined ? bridge.toPdf(source) : bridge.toPdf(bytes!, extension)
    void pending.then((result) => {
      if (cancelled) return
      setState('pdf' in result ? { pdf: result.pdf, application: result.application } : { code: result.code, error: result.error, missing: result.missing, encrypted: result.encrypted === true })
    }, (error: unknown) => { if (!cancelled) setState({ code: 'failed', error: errorMessage(error) }) })
    return () => { cancelled = true }
  }, [source?.workspaceId, source?.rootId, source?.path, source?.version, source?.extension, bytes, extension, attempt])
  return state
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

export function OfficePdf({ bytes, size, location, attempt, onRetry, fallback, modes }: {
  bytes?: Uint8Array | undefined
  size: number
  location: DocumentLocation
  attempt: number
  onRetry: () => void
  fallback?: ReactNode
  modes?: ReactNode
}): JSX.Element {
  const t = useViewerT()
  const source = pdfSourceOf(location)
  const result = useOfficePdf(source, source === undefined ? bytes : undefined, location.extension, attempt)
  const openInApp = useOpenInApp(location)
  if (result.encrypted === true) return <>{modes !== undefined && <div className={css.toolbar}>{modes}</div>}<Encrypted location={location} /></>
  if (result.pdf !== undefined) {
    return <PdfViewer data={result.pdf} bytes={bytes?.length ?? size} note={t('officeConvertedBy', { app: result.application ?? 'Office' })}
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
