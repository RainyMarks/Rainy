/**
 * One file shown alone (`/?view=…`), for the WebView2 panes of the native workbench: PDF, Office documents, media,
 * shortcuts and Markdown (rich editing and preview). The workbench keeps the file's text: it sends the text here and
 * receives edits, saves and opened links through `window.ipc`.
 */
import { lazy, Suspense, useEffect, useState } from 'react'
import type { IdeRootId, IdeWorkspace, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { ideRawUrl } from '../../shared/ide-preview.ts'
import { callIde, IdeRequestError } from '../ide/ide-api.ts'
import { ImageViewer } from '../ide/viewers/ImageViewer.tsx'
import { MarkdownPreview } from '../ide/viewers/MarkdownPreview.tsx'
import { MediaViewer } from '../ide/viewers/MediaViewer.tsx'
import { OfficeLockViewer, OfficeViewer, SheetViewer, ShortcutViewer, WordViewer, type DocumentLocation } from '../ide/viewers/OfficeViewers.tsx'
import { PdfViewer } from '../ide/viewers/PdfViewer.tsx'
import { RichMarkdown } from '../ide/viewers/RichMarkdown.tsx'
import { saveMarkdownImage } from '../ide/viewers/markdown-assets.ts'
import { ScrollSync } from '../ide/viewers/scroll-sync.ts'
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
  /** The user scrolled the split preview: the 1-based source line now at its top, possibly fractional. */
  | { type: 'scroll'; line: number }

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
      /** Scroll the Markdown preview to the source editor's first visible line: 1-based, possibly fractional. The page
       *  does not answer with a `scroll` message for the scrolling this causes. */
      scrollToLine(line: number): void
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
/** The source editor's scroll position, which the preview follows in split view. */
const editorScroll = new ScrollSync()
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
  scrollToLine(line) {
    if (Number.isFinite(line)) editorScroll.editorScrolled(line)
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
  throw new Error(viewerMessages.t('imageNoFreeName'))
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
  // The preview resolved the path against the document's directory already.
  if (!rich) {
    return <MarkdownPreview text={text.text} version={text.version} base={base} sync={editorScroll}
      openLocal={(target) => { post({ type: 'open-file', path: target.path, rootId, line: target.line }) }}
      onScrollLine={(line) => { post({ type: 'scroll', line }) }} />
  }
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

/** The project map of a workspace, as the 3.x workbench shows it in its center. */
function MapView({ workspaceId }: { workspaceId: WorkspaceId }): JSX.Element {
  const [workspace, setWorkspace] = useState<IdeWorkspace | null>(null)
  const [sessionId, setSessionId] = useState(latestSession)
  useEffect(() => {
    let cancelled = false
    void callIde({ op: 'workspaces.list' }).then((list) => {
      if (!cancelled) setWorkspace((list as readonly IdeWorkspace[]).find(item => item.workspaceId === workspaceId) ?? null)
    }, () => undefined)
    receiveSession = setSessionId
    return () => { cancelled = true; receiveSession = undefined }
  }, [workspaceId])
  const changes = useChanges(sessionId)
  const activity = useAiActivity()
  return <Suspense fallback={null}>
    <ProjectMapView workspace={workspace} visible changes={changes} activity={activity}
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
  useEffect(() => {
    if (view !== 'markdown' && view !== 'markdown-rich') post({ type: 'ready' })
  }, [view])
  let content: JSX.Element | null
  switch (view) {
    case 'image': case 'svg': content = <ImageViewer url={url} name={name} bytes={bytes} />; break
    case 'audio': case 'video': content = <MediaViewer url={url} kind={view} bytes={bytes} onHex={() => undefined} />; break
    case 'pdf': content = <PdfViewer url={url} bytes={bytes} />; break
    case 'word': content = <WordViewer url={url} size={bytes} location={documentLocation} />; break
    case 'sheet': content = <SheetViewer url={url} size={bytes} location={documentLocation} />; break
    case 'office': content = <OfficeViewer url={url} size={bytes} location={documentLocation} />; break
    case 'shortcut': content = <ShortcutViewer url={url} size={bytes} />; break
    case 'office-lock': content = <OfficeLockViewer url={url} size={bytes} document={name.slice(2)} onOpenDocument={undefined} />; break
    case 'markdown': case 'markdown-rich':
      content = <MarkdownView rich={view === 'markdown-rich'} workspaceId={workspaceId} rootId={rootId} path={path} />
      break
    case 'map': content = <MapView workspaceId={workspaceId} />; break
    default: content = null
  }
  return <div data-rainy-viewer={view} style={{ position: 'fixed', inset: 0, display: 'flex', flexDirection: 'column', background: 'var(--surface-editor, var(--background))' }}>
    {content}
    <ToastHost />
  </div>
}
