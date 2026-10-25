/**
 * One file shown alone (`/?view=…`), for the WebView2 panes of the native workbench: PDF, Office documents, images,
 * SVG, media, hex, shortcuts and Markdown (rich editing and preview). The workbench keeps the file's text: it sends the
 * text here and receives edits, saves, opened links, attachments and view changes through `window.ipc`.
 */
import { lazy, Suspense, useEffect, useState } from 'react'
import type { IdeDirectory, IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { ideRawUrl } from '../../shared/ide-preview.ts'
import { callIde, IdeRequestError } from '../ide/ide-api.ts'
import { lockedDocumentName } from '../ide/viewers/file-facts.ts'
import { HexViewer } from '../ide/viewers/HexViewer.tsx'
import { ImageViewer, type ImageAttachment } from '../ide/viewers/ImageViewer.tsx'
import { MarkdownPreview } from '../ide/viewers/MarkdownPreview.tsx'
import { MediaViewer } from '../ide/viewers/MediaViewer.tsx'
import { OfficeLockViewer, OfficeViewer, SheetViewer, ShortcutViewer, WordViewer, type DocumentLocation } from '../ide/viewers/OfficeViewers.tsx'
import { PdfViewer } from '../ide/viewers/PdfViewer.tsx'
import { RichMarkdown } from '../ide/viewers/RichMarkdown.tsx'
import { saveMarkdownImage } from '../ide/viewers/markdown-assets.ts'
import { viewerMessages } from '../ide/viewers/messages.ts'
import { outlinePosition, outlineTarget } from '../ide/outline-store.ts'
import { localTarget, resolveDocumentPath } from '../chat/Markdown.tsx'
import { getPrefs } from '../prefs.ts'
import { useAiActivity } from '../map/activity.ts'
import { useChanges } from '../review/changes-store.ts'
import { ToastHost } from '../ui/index.ts'

const ProjectMapView = lazy(async () => ({ default: (await import('../map/ProjectMap.tsx')).ProjectMapView }))

/** What the page tells the workbench. */
type Message =
  | { type: 'ready' }
  | { type: 'change'; text: string }
  | { type: 'save' }
  | { type: 'open-url'; url: string }
  | { type: 'open-file'; path: string; rootId?: string | undefined; line?: number | undefined }
  | { type: 'request'; id: number; name: string; params: unknown }
  | { type: 'open-key'; key: string }
  | { type: 'ask'; text: string }
  | { type: 'outline'; index: number }
  | { type: 'notice'; tone: 'success' | 'error'; text: string }
  | ({ type: 'attach' } & ImageAttachment)
  /** Show the tab as hex, or in its default view again. */
  | { type: 'view'; view: 'hex' | null }
  /** The file changed on disk while this page read it. */
  | { type: 'reload' }

declare global {
  interface Window {
    /** wry's message channel to the native workbench. */
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
    }
  }
}

function post(message: Message): void {
  window.ipc?.postMessage(JSON.stringify(message))
}

const pending = new Map<number, (result: unknown) => void>()
let nextRequest = 1

/** Ask the workbench to do what only the desktop can, such as converting a presentation with PowerPoint. */
function request(name: string, params: unknown): Promise<unknown> {
  const id = nextRequest++
  return new Promise((resolve) => {
    pending.set(id, resolve)
    post({ type: 'request', id, name, params })
  })
}

let receiveText: ((text: string, version: string | null) => void) | undefined
let latestText: { text: string; version: string | null } | undefined
let receiveSession: ((sessionId: string | null) => void) | undefined
let latestSession: string | null = null
/** The Markdown document this page shows, for exporting it. */
let shownDocument: { workspaceId: WorkspaceId; rootId: IdeRootId | undefined; path: string } | undefined
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
    pending.get(id)?.(result)
    pending.delete(id)
  },
  revealHeading(index) {
    if (shownDocument !== undefined) outlineTarget(shownDocument.path)?.reveal(index)
  },
  exportDocument(format) {
    const shown = shownDocument
    if (shown === undefined || latestText === undefined) return
    const text = latestText.text
    void import('../ide/viewers/markdown-export.ts')
      .then(({ exportDocument }) => exportDocument({ format, text, path: shown.path, workspaceId: shown.workspaceId, rootId: shown.rootId }))
      .then((saved) => { if (saved !== null) post({ type: 'notice', tone: 'success', text: viewerMessages.t('exportDone', { path: saved }) }) },
        (error: unknown) => { post({ type: 'notice', tone: 'error', text: viewerMessages.t('exportFailed', { error: error instanceof Error ? error.message : String(error) }) }) })
  },
}
// The heading at the top of the rich editor, for the outline beside it.
outlinePosition.subscribe(() => { post({ type: 'outline', index: outlinePosition.getSnapshot().index }) })

if (window.ipc !== undefined) {
  window.__RAINY_OFFICE__ = {
    toPdf: async (bytes: Uint8Array, extension: string) => {
      let binary = ''
      for (let offset = 0; offset < bytes.length; offset += 0x8000) binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000))
      const result = await request('office-pdf', { base64: btoa(binary), extension }) as { pdf?: string; [key: string]: unknown }
      if (typeof result.pdf !== 'string') return result as never
      const raw = atob(result.pdf)
      const pdf = new Uint8Array(raw.length)
      for (let index = 0; index < raw.length; index++) pdf[index] = raw.charCodeAt(index)
      return { ...result, pdf } as never
    },
    open: async (path: string) => await request('open-external', { path }) as { error?: string },
  } as never
  window.__RAINY_EXPORT__ = {
    save: async (options) => {
      const result = await request('export-save', options) as { path?: string; error?: string }
      if (typeof result.error === 'string') throw new Error(result.error)
      return typeof result.path === 'string' ? { path: result.path } : null
    },
    showInFolder: async (path) => { await request('open-external', { path, reveal: true }) },
  }
}

/** Save an image pasted into the rich editor next to the document, as the editor tab does. */
async function createBinary(workspaceId: WorkspaceId, rootId: IdeRootId | undefined, folder: string, name: string, bytes: Uint8Array): Promise<string> {
  if (folder !== '') {
    try { await callIde({ op: 'files.mkdir', workspaceId, rootId, path: folder }) } catch (error) {
      if (!(error instanceof IdeRequestError && error.code === 'already-exists')) throw error
    }
  }
  let binary = ''
  for (let offset = 0; offset < bytes.length; offset += 0x8000) binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000))
  const base64 = btoa(binary)
  const dot = name.lastIndexOf('.')
  const [stem, extension] = dot > 0 ? [name.slice(0, dot), name.slice(dot)] : [name, '']
  for (let attempt = 0; attempt < 100; attempt++) {
    const candidate = attempt === 0 ? name : `${stem}-${attempt}${extension}`
    const path = folder === '' ? candidate : `${folder}/${candidate}`
    try {
      await callIde({ op: 'files.createBinary', workspaceId, rootId, path, base64 })
      return path
    } catch (error) {
      if (error instanceof IdeRequestError && error.code === 'already-exists') continue
      throw error
    }
  }
  throw new Error('No free image name.')
}

/** The Markdown views: the text arrives from the workbench, and rich edits go back to it. */
function MarkdownView({ rich, workspaceId, rootId, path }: { rich: boolean; workspaceId: WorkspaceId; rootId: IdeRootId | undefined; path: string }): JSX.Element | null {
  const [text, setText] = useState(latestText)
  shownDocument = { workspaceId, rootId, path }
  useEffect(() => {
    receiveText = (value, version) => { setText({ text: value, version }) }
    post({ type: 'ready' })
    return () => { receiveText = undefined }
  }, [])
  if (text === undefined) return null
  const directory = path.split('/').slice(0, -1).join('/')
  const base = { workspaceId, rootId, directory }
  if (!rich) return <MarkdownPreview text={text.text} version={text.version} base={base} />
  return <RichMarkdown path={path} text={text.text} version={text.version} base={base}
    onChange={(value) => { latestText = { text: value, version: text.version }; post({ type: 'change', text: value }) }}
    onSave={() => { post({ type: 'save' }) }}
    onImage={file => saveMarkdownImage((folder, name, bytes) => createBinary(workspaceId, rootId, folder, name, bytes), path, file, getPrefs().markdownImages)}
    onOpenLink={(href) => {
      if (/^(?:https?|mailto):/iu.test(href)) { post({ type: 'open-url', url: href }); return }
      const target = localTarget(href)
      const resolved = target === undefined ? undefined : resolveDocumentPath(directory, target.path)
      if (target !== undefined && resolved !== undefined) post({ type: 'open-file', path: resolved, rootId, line: target.line })
    }} />
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
    post({ type: 'ready' })
    return () => { receiveText = undefined }
  }, [])
  useEffect(() => {
    if (text === undefined) return
    const timer = window.setTimeout(() => { setLive(URL.createObjectURL(new Blob([text.text], { type: 'image/svg+xml' }))) }, 150)
    return () => { window.clearTimeout(timer) }
  }, [text])
  useEffect(() => () => { if (live !== undefined) URL.revokeObjectURL(live) }, [live])
  return <ImageViewer url={live ?? url} name={name} bytes={text === undefined ? bytes : new Blob([text.text]).size} onAttach={attachImage} />
}

/** An Office `~$` owner file, with a button for the document it belongs to when that is in the same folder. */
function OfficeLockView({ url, bytes, workspaceId, rootId, path }: { url: string; bytes: number; workspaceId: WorkspaceId; rootId: IdeRootId | undefined; path: string }): JSX.Element {
  const slash = path.lastIndexOf('/')
  const folder = slash < 0 ? '' : path.slice(0, slash)
  const lock = path.slice(slash + 1)
  const [locked, setLocked] = useState<string | undefined>()
  useEffect(() => {
    let cancelled = false
    void callIde({ op: 'files.list', workspaceId, rootId, path: folder }).then((listing) => {
      if (!cancelled) setLocked(lockedDocumentName(lock, (listing as IdeDirectory).entries))
    }, () => undefined)
    return () => { cancelled = true }
  }, [workspaceId, rootId, folder, lock])
  return <OfficeLockViewer url={url} size={bytes} document={locked ?? lock.slice(2)}
    onOpenDocument={locked === undefined ? undefined : () => { post({ type: 'open-file', path: folder === '' ? locked : `${folder}/${locked}`, rootId }) }} />
}

/** The project map of a workspace, as the 3.x workbench shows it in its center. */
function MapView({ workspaceId }: { workspaceId: WorkspaceId }): JSX.Element {
  const [sessionId, setSessionId] = useState(latestSession)
  useEffect(() => {
    receiveSession = setSessionId
    return () => { receiveSession = undefined }
  }, [])
  const changes = useChanges(sessionId)
  const activity = useAiActivity()
  return <Suspense fallback={null}>
    <ProjectMapView workspaceId={workspaceId} visible changes={changes} activity={activity}
      openFile={(key) => { post({ type: 'open-key', key }) }} ask={(text) => { post({ type: 'ask', text }) }} />
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
  const bytes = Number(params.get('bytes') ?? '0')
  const name = path.split('/').at(-1) ?? path
  const hostPath = params.get('hostPath') ?? undefined
  const url = ideRawUrl({ workspaceId, rootId, path, version })
  const documentLocation: DocumentLocation = { name, extension: name.slice(name.lastIndexOf('.') + 1).toLowerCase(), hostPath }
  // Pages of the editor's text say they are ready once they can take it.
  useEffect(() => {
    if (view !== 'markdown' && view !== 'markdown-rich' && view !== 'svg') post({ type: 'ready' })
  }, [view])
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
    case 'office-lock': content = <OfficeLockView url={url} bytes={bytes} workspaceId={workspaceId} rootId={rootId} path={path} />; break
    case 'markdown': case 'markdown-rich':
      content = <MarkdownView rich={view === 'markdown-rich'} workspaceId={workspaceId} rootId={rootId} path={path} />
      break
    case 'map': content = <MapView workspaceId={workspaceId} />; break
    default: content = null
  }
  return <div data-rainy-viewer={view} style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', background: 'var(--rainy-alias-bg-base)' }}>
    {content}
    <ToastHost />
  </div>
}
