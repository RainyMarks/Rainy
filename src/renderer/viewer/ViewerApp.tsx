/**
 * One file shown alone (`/?view=…`), for the WebView2 panes of the native workbench: PDF, Office documents, images,
 * SVG, media, hex, shortcuts and Markdown (rich editing and preview). The workbench keeps the file's text: it sends the
 * text here and receives edits, saves, opened links, attachments and view changes through `window.ipc`.
 */
import { lazy, Suspense, useEffect, useState } from 'react'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { ideOfficePdfUrl, ideRawUrl, isSameOriginOfficePdfUrl } from '../../shared/ide-preview.ts'
import type { OfficePdfResult, OfficePdfSource } from '../../shared/office-protocol.ts'
import type { ImageAttachment } from '../ide/viewers/ImageViewer.tsx'
import type { DocumentLocation } from '../ide/viewers/office-shared.tsx'
import { ScrollSync } from '../ide/viewers/scroll-sync.ts'
import { encodeBase64, errorMessage } from '../ide/viewers/viewer-utils.ts'
import { outlinePosition, outlineTarget } from '../ide/outline-store.ts'
import { getPrefs } from '../prefs.ts'
import { ToastHost } from '../ui/toasts.tsx'

const ImageViewer = lazy(async () => ({ default: (await import('../ide/viewers/ImageViewer.tsx')).ImageViewer }))
const HexViewer = lazy(async () => ({ default: (await import('../ide/viewers/HexViewer.tsx')).HexViewer }))
const MediaViewer = lazy(async () => ({ default: (await import('../ide/viewers/MediaViewer.tsx')).MediaViewer }))
const PdfViewer = lazy(async () => ({ default: (await import('../ide/viewers/PdfViewer.tsx')).PdfViewer }))
const WordViewer = lazy(async () => ({ default: (await import('../ide/viewers/WordViewer.tsx')).WordViewer }))
const SheetViewer = lazy(async () => ({ default: (await import('../ide/viewers/SheetViewer.tsx')).SheetViewer }))
const OfficeViewer = lazy(async () => ({ default: (await import('../ide/viewers/OfficeViewers.tsx')).OfficeViewer }))
const ShortcutViewer = lazy(async () => ({ default: (await import('../ide/viewers/OfficeViewers.tsx')).ShortcutViewer }))
const OfficeLockPane = lazy(async () => ({ default: (await import('./OfficeLockPane.tsx')).OfficeLockPane }))
const MarkdownPane = lazy(async () => ({ default: (await import('./MarkdownPane.tsx')).MarkdownPane }))
const MapPane = lazy(async () => ({ default: (await import('./MapPane.tsx')).MapPane }))

/** What the page tells the workbench. */
type Message =
  | { type: 'ready' }
  | { type: 'change'; text: string }
  | { type: 'save' }
  | { type: 'open-url'; url: string }
  | { type: 'open-file'; path: string; rootId?: string | undefined; line?: number | undefined }
  | { type: 'request'; id: number; name: string; params: unknown }
  | { type: 'open-key'; key: string; line?: number | undefined }
  | { type: 'ask'; text: string }
  | { type: 'outline'; index: number }
  | { type: 'notice'; tone: 'success' | 'error'; text: string }
  | ({ type: 'attach' } & ImageAttachment)
  /** Show the tab as hex or, for a document too large for the rich view, its source; or its default view again. */
  | { type: 'view'; view: 'hex' | 'source' | null }
  /** The file changed on disk while this page read it. */
  | { type: 'reload' }
  /** The user scrolled the split preview: the 1-based source line now at its top, possibly fractional. */
  | { type: 'scroll'; line: number }
  /** A workbench key sequence pressed in the page, whose second key the window never sees. */
  | { type: 'chord'; keys: 'ctrl-k v' }

declare global {
  interface Window {
    /** WebView2's message channel to the native workbench. */
    ipc?: { postMessage(message: string): void }
    /** What the workbench calls in the page. */
    __rainyViewer?: {
      setText(text: string, version: string | null): void
      resolve(id: number, result: unknown): void
      /** The chat shown beside the project map, whose changes the map marks. */
      setSession(sessionId: string | null): void
      /** Scroll the rich editor to a heading, in the order the outline lists them. */
      revealHeading(index: number): void
      /** Save the Markdown document as a PDF or a self-contained HTML page. */
      exportDocument(format: 'pdf' | 'html'): void
      /** Scroll the Markdown preview to the source editor's first visible line: 1-based, possibly fractional. The page
       *  does not answer with a `scroll` message for the scrolling this causes. */
      scrollToLine(line: number): void
    }
  }
}

function post(message: Message): void {
  window.ipc?.postMessage(JSON.stringify(message))
}

interface PendingIpc {
  readonly resolve: (result: unknown) => void
  readonly reject: (error: Error) => void
  readonly timer: ReturnType<typeof setTimeout>
}

const pending = new Map<number, PendingIpc>()
let nextRequest = 1

function requestTimeoutMs(name: string): number {
  if (name === 'office-pdf') return 150_000
  if (name === 'export-save') return 600_000
  return 60_000
}

/** Ask the workbench to do what only the desktop can, such as converting a presentation with PowerPoint. */
function request(name: string, params: unknown): Promise<unknown> {
  if (window.ipc === undefined) return Promise.reject(new Error('Desktop IPC is unavailable'))
  const id = nextRequest++
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      if (!pending.delete(id)) return
      reject(new Error(`Desktop request "${name}" timed out`))
    }, requestTimeoutMs(name))
    pending.set(id, { resolve, reject, timer })
    post({ type: 'request', id, name, params })
  })
}

let receiveText: ((text: string, version: string | null) => void) | undefined
let latestText: { text: string; version: string | null } | undefined
let receiveSession: ((sessionId: string | null) => void) | undefined
let latestSession: string | null = null
/** Prevents React StrictMode double-mount from posting duplicate `ready` signals for the same view. */
let readyPostedKey: string | undefined
/** The Markdown document this page shows, for exporting it. */
let shownDocument: { workspaceId: WorkspaceId; rootId: IdeRootId | undefined; path: string; hostRoot: string | undefined } | undefined
/** The source editor's scroll position, which the preview follows in split view. */
const editorScroll = new ScrollSync()

function postReadyOnce(key: string): void {
  if (readyPostedKey === key) return
  readyPostedKey = key
  post({ type: 'ready' })
}

window.__rainyViewer = {
  setSession(sessionId) {
    latestSession = sessionId
    receiveSession?.(sessionId)
  },
  setText(text, version) {
    latestText = { text, version }
    receiveText?.(text, version)
  },
  resolve(id, result) {
    const entry = pending.get(id)
    if (entry === undefined) return
    pending.delete(id)
    clearTimeout(entry.timer)
    entry.resolve(result)
  },
  revealHeading(index) {
    if (shownDocument !== undefined) outlineTarget(shownDocument.path)?.reveal(index)
  },
  exportDocument(format) {
    const shown = shownDocument
    if (shown === undefined || latestText === undefined) return
    const text = latestText.text
    const locale = getPrefs().locale
    void Promise.all([import('../ide/viewers/markdown-export.ts'), import('../ide/viewers/messages.ts')])
      .then(([{ exportDocument }, { viewerMessages }]) =>
        exportDocument({ format, text, path: shown.path, workspaceId: shown.workspaceId, rootId: shown.rootId, hostRoot: shown.hostRoot, locale })
          .then((saved) => { if (saved !== null) post({ type: 'notice', tone: 'success', text: viewerMessages.tFor(locale, 'exportDone', { path: saved }) }) },
            (error: unknown) => { post({ type: 'notice', tone: 'error', text: viewerMessages.tFor(locale, 'exportFailed', { error: errorMessage(error) }) }) }),
      )
  },
  scrollToLine(line) {
    if (Number.isFinite(line)) editorScroll.editorScrolled(line)
  },
}
// The heading at the top of the rich editor, for the outline beside it.
outlinePosition.subscribe(() => { post({ type: 'outline', index: outlinePosition.getSnapshot().index }) })

/** Shape returned by the desktop `office-pdf` IPC request (zero-Base64 one-shot URL or legacy Base64 fallback). */
export interface OfficePdfIpcResponse {
  readonly url?: string | undefined
  readonly token?: string | undefined
  readonly pdf?: string | undefined
  readonly application?: string | undefined
  readonly code?: string | undefined
  readonly error?: string | undefined
  readonly missing?: boolean | undefined
  readonly encrypted?: boolean | undefined
}

/**
 * Consume the desktop's `office-pdf` IPC response: prefers the Host's one-shot binary route
 * (`/rainy/ide/office-pdf?token=...`) via `fetch` + `ArrayBuffer` (zero Base64), with fallback for legacy payloads.
 * @param result Raw IPC response object.
 * @param fetcher Fetch implementation (injectable for tests).
 * @returns Binary PDF bytes or the structured conversion failure.
 */
export async function resolveOfficePdfResponse(
  result: OfficePdfIpcResponse,
  fetcher: typeof fetch = fetch,
): Promise<OfficePdfResult> {
  const candidateUrl = typeof result.url === 'string' && result.url !== ''
    ? result.url
    : typeof result.token === 'string' && result.token !== ''
      ? ideOfficePdfUrl(result.token)
      : undefined
  if (candidateUrl !== undefined) {
    if (!isSameOriginOfficePdfUrl(candidateUrl)) {
      return { code: 'failed', error: 'Invalid office-pdf URL', missing: false }
    }
    const response = await fetcher(candidateUrl, { credentials: 'include', cache: 'no-store' })
    if (!response.ok) {
      return { code: 'failed', error: `HTTP ${response.status}`, missing: false }
    }
    const pdf = new Uint8Array(await response.arrayBuffer())
    return { pdf, application: typeof result.application === 'string' ? result.application : 'Office' }
  }
  if (typeof result.pdf === 'string') {
    let raw: string
    try { raw = atob(result.pdf) } catch { return { code: 'failed', error: 'Invalid base64 PDF payload', missing: false } }
    const pdf = new Uint8Array(raw.length)
    for (let index = 0; index < raw.length; index++) pdf[index] = raw.charCodeAt(index)
    return { pdf, application: typeof result.application === 'string' ? result.application : 'Office' }
  }
  return result as OfficePdfResult
}

if (window.ipc !== undefined) {
  window.__RAINY_OFFICE__ = {
    toPdf: async (sourceOrBytes: OfficePdfSource | Uint8Array, extension?: string) => {
      const params = sourceOrBytes instanceof Uint8Array
        ? { base64: encodeBase64(sourceOrBytes), extension: extension ?? '' }
        : {
            workspaceId: sourceOrBytes.workspaceId,
            rootId: sourceOrBytes.rootId,
            path: sourceOrBytes.path,
            version: sourceOrBytes.version,
            extension: sourceOrBytes.extension,
          }
      const result = await request('office-pdf', params) as OfficePdfIpcResponse
      return await resolveOfficePdfResponse(result)
    },
    open: async (path: string) => await request('open-external', { path }) as { error?: string },
  }
  window.__RAINY_EXPORT__ = {
    save: async (options) => {
      const result = await request('export-save', options) as { path?: string; error?: string }
      if (typeof result.error === 'string') throw new Error(result.error)
      return typeof result.path === 'string' ? { path: result.path } : null
    },
  }
}

/** How long after Ctrl+K the V of Ctrl+K V may follow, as GPUI waits for the rest of a key sequence. */
const CHORD_MS = 1500

/**
 * VS Code's Ctrl+K V in the preview: the workbench shows the source beside it, or the preview alone again. The window
 * sees Ctrl+K but never the V, which only this page receives, so the page reports the sequence. The rich view keeps
 * Ctrl+K for links, as Typora does.
 */
function usePreviewChord(enabled: boolean): void {
  useEffect(() => {
    if (!enabled) return
    let armed = 0
    const keydown = (event: KeyboardEvent): void => {
      if (event.isComposing || ['Control', 'Shift', 'Alt', 'Meta'].includes(event.key)) return
      const key = event.key.toLowerCase()
      const plain = !event.ctrlKey && !event.shiftKey && !event.altKey && !event.metaKey
      if (armed !== 0 && Date.now() - armed < CHORD_MS && plain && key === 'v') {
        armed = 0
        event.preventDefault()
        post({ type: 'chord', keys: 'ctrl-k v' })
        return
      }
      armed = event.ctrlKey && !event.shiftKey && !event.altKey && !event.metaKey && key === 'k' ? Date.now() : 0
      if (armed !== 0) event.preventDefault()
    }
    window.addEventListener('keydown', keydown)
    return () => { window.removeEventListener('keydown', keydown) }
  }, [enabled])
}

/** The Markdown views: the text arrives from the workbench, and rich edits go back to it. */
function MarkdownView({ rich, workspaceId, rootId, path, hostPath }: {
  rich: boolean
  workspaceId: WorkspaceId
  rootId: IdeRootId | undefined
  path: string
  /** The document on this computer, when the project is a Windows folder. */
  hostPath: string | undefined
}): JSX.Element | null {
  const [text, setText] = useState(latestText)
  const directory = path.split('/').slice(0, -1).join('/')
  // The project folder on this computer: the document's own path without its project-relative part.
  const suffix = `\\${path.replaceAll('/', '\\')}`
  const hostRoot = hostPath?.toLowerCase().endsWith(suffix.toLowerCase()) === true ? hostPath.slice(0, -suffix.length) : undefined
  usePreviewChord(!rich)
  useEffect(() => {
    shownDocument = { workspaceId, rootId, path, hostRoot }
    return () => { shownDocument = undefined }
  }, [workspaceId, rootId, path, hostRoot])
  useEffect(() => {
    receiveText = (value, version) => { setText({ text: value, version }) }
    if (latestText !== undefined) setText(latestText)
    postReadyOnce(`markdown:${rich ? 'rich' : 'preview'}:${path}`)
    return () => { receiveText = undefined }
  }, [rich, path])
  if (text === undefined) return null
  return (
    <Suspense fallback={null}>
      <MarkdownPane rich={rich} workspaceId={workspaceId} rootId={rootId} path={path} directory={directory} hostRoot={hostRoot}
        text={text.text} version={text.version} editorScroll={editorScroll}
        onChange={(value) => { latestText = { text: value, version: text.version }; post({ type: 'change', text: value }) }}
        onSave={() => { post({ type: 'save' }) }}
        onSource={() => { post({ type: 'view', view: 'source' }) }}
        onOpenUrl={(url) => { post({ type: 'open-url', url }) }}
        onOpenFile={(targetPath, line) => { post({ type: 'open-file', path: targetPath, rootId, line }) }}
        onScrollLine={(line) => { post({ type: 'scroll', line }) }} />
    </Suspense>
  )
}

/** "Add to chat" of the image viewer, where the workbench is there to take the image. */
const attachImage = window.ipc === undefined ? undefined : (image: ImageAttachment): void => { post({ type: 'attach', ...image }) }

/** An SVG file as the picture of the editor's current text, which the workbench sends as it changes. Before it arrives
 *  the saved file is shown. SVG shown through <img> never runs its scripts, so unsaved text is shown as it is. */
function SvgView({ url, name, bytes }: { url: string; name: string; bytes: number }): JSX.Element {
  const [text, setText] = useState(latestText)
  const [live, setLive] = useState<string | undefined>()
  useEffect(() => {
    receiveText = (value, version) => { setText({ text: value, version }) }
    if (latestText !== undefined) setText(latestText)
    postReadyOnce(`svg:${url}`)
    return () => { receiveText = undefined }
  }, [url])
  useEffect(() => {
    if (text === undefined) return
    const timer = window.setTimeout(() => { setLive(URL.createObjectURL(new Blob([text.text], { type: 'image/svg+xml' }))) }, 150)
    return () => { window.clearTimeout(timer) }
  }, [text])
  useEffect(() => () => { if (live !== undefined) URL.revokeObjectURL(live) }, [live])
  return <Suspense fallback={null}>
    <ImageViewer url={live ?? url} name={name} bytes={text === undefined ? bytes : new Blob([text.text]).size} onAttach={attachImage} />
  </Suspense>
}

/** The project map of a workspace, as the 3.x workbench shows it in its center. */
function MapView({ workspaceId }: { workspaceId: WorkspaceId }): JSX.Element {
  const [sessionId, setSessionId] = useState(latestSession)
  useEffect(() => {
    receiveSession = setSessionId
    if (latestSession !== null) setSessionId(latestSession)
    return () => { receiveSession = undefined }
  }, [])
  return <Suspense fallback={null}>
    <MapPane workspaceId={workspaceId} sessionId={sessionId}
      openFile={(key, line) => { post({ type: 'open-key', key, line }) }}
      ask={(text) => { post({ type: 'ask', text }) }} />
  </Suspense>
}

/** @returns The viewer the address names. */
export function ViewerApp(): JSX.Element {
  const params = new URLSearchParams(window.location.search)
  const view = params.get('view') ?? ''
  const workspaceId = (params.get('workspace') ?? '') as WorkspaceId
  const rootId = (params.get('root') ?? undefined) as IdeRootId | undefined
  const path = params.get('path') ?? ''
  const version = params.get('version') ?? undefined
  const rawBytes = Number(params.get('bytes') ?? '0')
  const bytes = Number.isFinite(rawBytes) && rawBytes >= 0 ? rawBytes : 0
  const name = path.split('/').at(-1) ?? path
  const dot = name.lastIndexOf('.')
  const hostPath = params.get('hostPath') ?? undefined
  const url = ideRawUrl({ workspaceId, rootId, path, version })
  const documentLocation: DocumentLocation = {
    name,
    extension: dot > 0 ? name.slice(dot + 1).toLowerCase() : '',
    hostPath,
    workspaceId,
    rootId,
    path,
    version,
  }
  // Pages of the editor's text say they are ready once they can take it.
  useEffect(() => {
    if (view !== 'markdown' && view !== 'markdown-rich' && view !== 'svg') postReadyOnce(`view:${view}:${path}`)
  }, [view, path])
  let content: JSX.Element | null
  switch (view) {
    case 'image': content = <ImageViewer url={url} name={name} bytes={bytes} onAttach={attachImage} />; break
    case 'svg': content = <SvgView url={url} name={name} bytes={bytes} />; break
    case 'hex':
      content = <HexViewer url={url} size={bytes} onChanged={() => { post({ type: 'reload' }) }}
        onBack={params.get('back') === '1' ? () => { post({ type: 'view', view: null }) } : undefined} />
      break
    case 'audio': case 'video': content = <MediaViewer url={url} kind={view} bytes={bytes} onHex={() => { post({ type: 'view', view: 'hex' }) }} />; break
    case 'pdf': content = <PdfViewer url={url} bytes={bytes} />; break
    case 'word': content = <WordViewer url={url} size={bytes} location={documentLocation} />; break
    case 'sheet': content = <SheetViewer url={url} size={bytes} location={documentLocation} />; break
    case 'office': content = <OfficeViewer url={url} size={bytes} location={documentLocation} />; break
    case 'shortcut': content = <ShortcutViewer url={url} size={bytes} />; break
    case 'office-lock':
      content = <OfficeLockPane url={url} bytes={bytes} workspaceId={workspaceId} rootId={rootId} path={path}
        onOpenFile={(target) => { post({ type: 'open-file', path: target, rootId }) }} />
      break
    case 'markdown': case 'markdown-rich':
      content = <MarkdownView rich={view === 'markdown-rich'} workspaceId={workspaceId} rootId={rootId} path={path} hostPath={hostPath} />
      break
    case 'map': content = <MapView workspaceId={workspaceId} />; break
    default: content = null
  }
  return <div data-rainy-viewer={view} style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', background: 'var(--rainy-alias-bg-base)' }}>
    <Suspense fallback={null}>{content}</Suspense>
    <ToastHost />
  </div>
}
