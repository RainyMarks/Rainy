/** The viewer half of an editor tab: images, SVG, Markdown, PDF, office documents, audio, video, shortcuts, hex and oversized text. */
import { useEffect, useState } from 'react'
import type { IdeWorkspace } from '../../../shared/ide-files-protocol.ts'
import { ideRawUrl } from '../../../shared/ide-preview.ts'
import type { IdeBuffer, IdeModel } from '../ide-model.ts'
import { fileKey, fileReference } from '../ide-paths.ts'
import { HexViewer } from './HexViewer.tsx'
import { ImageViewer } from './ImageViewer.tsx'
import { MarkdownPreview } from './MarkdownPreview.tsx'
import { MediaViewer } from './MediaViewer.tsx'
import { OfficeLockViewer, OfficeViewer, SheetViewer, ShortcutViewer, WordViewer, type DocumentLocation } from './OfficeViewers.tsx'
import { useViewerT } from './messages.ts'
import { PdfViewer } from './PdfViewer.tsx'
import type { ScrollSync } from './scroll-sync.ts'
import type { ViewerChoice } from './viewer-choice.ts'
import css from './Viewers.module.css'

/** SVG from the saved file, or from the unsaved buffer while it is being edited. */
function SvgPreview({ url, text, dirty, name, bytes }: {
  url: string
  text: string
  dirty: boolean
  name: string
  bytes: number
}): JSX.Element {
  const [live, setLive] = useState<string | undefined>()
  useEffect(() => {
    if (!dirty) { setLive(undefined); return }
    // SVG shown through <img> never runs its scripts, so the unsaved text can be previewed as is.
    const timer = window.setTimeout(() => { setLive(URL.createObjectURL(new Blob([text], { type: 'image/svg+xml' }))) }, 150)
    return () => { window.clearTimeout(timer) }
  }, [text, dirty])
  useEffect(() => () => { if (live !== undefined) URL.revokeObjectURL(live) }, [live])
  return <ImageViewer url={live ?? url} name={name} bytes={dirty ? new Blob([text]).size : bytes} />
}

/** Host path of a project file, in the spelling of the root it lives in. */
function hostPath(workspace: IdeWorkspace, rootId: string | undefined, path: string): string {
  const root = workspace.roots?.find(value => value.rootId === rootId) ?? workspace.roots?.find(value => value.primary)
  const base = root?.path ?? workspace.path
  const separator = base.includes('\\') ? '\\' : '/'
  return `${base.replace(/[\\/]+$/u, '')}${separator}${path.split('/').join(separator)}`
}

/** The document an Office `~$` owner file belongs to, from the listing of its folder: Office keeps the name when it is
 *  short and replaces its first two characters with `~$` otherwise. */
function lockedDocument(model: IdeModel, path: string): { path: string; name: string } | undefined {
  const reference = fileReference(path)
  const slash = reference.path.lastIndexOf('/')
  const folder = fileKey(slash < 0 ? '' : reference.path.slice(0, slash), reference.rootId)
  const lock = reference.path.slice(slash + 1)
  const rest = lock.slice(2)
  const entries = model.state.getSnapshot().directories[folder]?.entries ?? []
  return entries.find(entry => entry.kind === 'file' && entry.name === rest)
    ?? entries.find(entry => entry.kind === 'file' && entry.name !== lock && entry.name.length === lock.length && entry.name.endsWith(rest) && !entry.name.startsWith('~$'))
}

function LargeText({ buffer, onHex }: { buffer: IdeBuffer; onHex: () => void }): JSX.Element {
  const t = useViewerT()
  return (
    <div className={css.viewer} data-viewer="large">
      <div className={css.notice}>
        <span>{t('readOnlyLarge')}</span>
        <button type="button" className={css.toolButton} onClick={onHex}>{t('viewAsHex')}</button>
      </div>
      {buffer.document.preview !== undefined && <pre className={css.prefix}>{buffer.document.preview.text}</pre>}
    </div>
  )
}

/**
 * @param props.workspace Project of the tab.
 * @param props.path File key.
 * @param props.buffer Open buffer with the Host's read result.
 * @param props.choice Viewer selected for the tab.
 * @param props.model Workspace model, for view changes, reloads and decoding.
 * @param props.sync Source editor scroll positions, for the split Markdown preview.
 * @param props.onScrollLine The split preview scrolled; the editor follows.
 * @returns The viewer, placed in the right half when the tab is split.
 */
export function FileViewer({ workspace, path, buffer, choice, model, sync, onScrollLine }: {
  workspace: IdeWorkspace
  path: string
  buffer: IdeBuffer
  choice: ViewerChoice
  model: IdeModel
  sync: ScrollSync
  onScrollLine: (line: number) => void
}): JSX.Element | null {
  const reference = fileReference(path)
  const version = buffer.document.version ?? undefined
  const url = ideRawUrl({ workspaceId: workspace.workspaceId, rootId: reference.rootId, path: reference.path, version })
  const name = reference.path.split('/').at(-1) ?? reference.path
  const bytes = buffer.document.bytes
  const hex = (): void => { model.setView(path, 'hex') }
  const location: DocumentLocation = {
    name, extension: name.slice(name.lastIndexOf('.') + 1).toLowerCase(),
    hostPath: window.__RAINY_AGENT__?.environment === 'SSH' ? undefined : hostPath(workspace, reference.rootId, reference.path),
  }
  let content: JSX.Element
  switch (choice.viewer) {
    case undefined: return null
    case 'image': content = <ImageViewer key={url} url={url} name={name} bytes={bytes} />; break
    case 'svg': content = <SvgPreview key={path} url={url} text={buffer.text} dirty={buffer.dirty} name={name} bytes={bytes} />; break
    case 'audio':
    case 'video': content = <MediaViewer key={url} url={url} kind={choice.viewer} bytes={bytes} onHex={hex} />; break
    case 'pdf': content = <PdfViewer key={url} url={url} bytes={bytes} />; break
    case 'word': content = <WordViewer key={url} url={url} size={bytes} location={location} />; break
    case 'sheet': content = <SheetViewer key={url} url={url} size={bytes} location={location} />; break
    case 'office': content = <OfficeViewer key={url} url={url} size={bytes} location={location} />; break
    case 'shortcut': content = <ShortcutViewer key={url} url={url} size={bytes} />; break
    case 'office-lock': {
      const document = lockedDocument(model, path)
      content = <OfficeLockViewer key={url} url={url} size={bytes} document={document?.name ?? name.slice(2)}
        onOpenDocument={document === undefined ? undefined : () => { void model.openFile(document.path).catch((error: unknown) => { model.fail(error) }) }} />
      break
    }
    case 'large': content = <LargeText buffer={buffer} onHex={hex} />; break
    case 'hex':
      content = <HexViewer key={url} url={url} size={bytes}
        onChanged={() => { if (!buffer.dirty) void model.reload(path).catch((error: unknown) => { model.fail(error) }) }}
        onOpenText={(encoding) => {
          void model.decodeAs(path, encoding).then(() => { model.setView(path, undefined) }, (error: unknown) => { model.fail(error) })
        }} />
      break
    case 'markdown':
      content = <MarkdownPreview key={path} text={buffer.text} version={buffer.document.version}
        base={{ workspaceId: workspace.workspaceId, rootId: reference.rootId, directory: reference.path.split('/').slice(0, -1).join('/') }}
        sync={choice.split ? sync : undefined} onScrollLine={choice.split ? onScrollLine : undefined} />
      break
  }
  return choice.split ? <div className={css.rightPane}>{content}</div> : content
}
