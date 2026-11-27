/** Lazy-loaded Markdown pane (preview and rich editor): keeps `react-markdown`, `remark-*` and Crepe out of the main entry chunk. */
import { lazy, Suspense, useMemo } from 'react'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { localTarget, resolveDocumentPath } from '../chat/Markdown.tsx'
import { callIde, IdeRequestError } from '../ide/ide-api.ts'
import { saveMarkdownImage } from '../ide/viewers/markdown-assets.ts'
import { viewerMessages } from '../ide/viewers/messages.ts'
import type { ScrollSync } from '../ide/viewers/scroll-sync.ts'
import { encodeBase64 } from '../ide/viewers/viewer-utils.ts'
import { getPrefs } from '../prefs.ts'

const MarkdownPreview = lazy(async () => ({ default: (await import('../ide/viewers/MarkdownPreview.tsx')).MarkdownPreview }))
const RichMarkdown = lazy(async () => ({ default: (await import('../ide/viewers/RichMarkdown.tsx')).RichMarkdown }))

/** Save an image pasted into the rich editor next to the document, as the editor tab does. */
async function createBinary(workspaceId: WorkspaceId, rootId: IdeRootId | undefined, folder: string, name: string, bytes: Uint8Array): Promise<string> {
  if (folder !== '') {
    try { await callIde({ op: 'files.mkdir', workspaceId, rootId, path: folder }) } catch (error) {
      if (!(error instanceof IdeRequestError && error.code === 'already-exists')) throw error
    }
  }
  const base64 = encodeBase64(bytes)
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

export function MarkdownPane({ rich, workspaceId, rootId, path, directory, hostRoot, text, version, editorScroll, onChange, onSave, onSource, onOpenUrl, onOpenFile, onScrollLine }: {
  rich: boolean
  workspaceId: WorkspaceId
  rootId: IdeRootId | undefined
  path: string
  directory: string
  hostRoot: string | undefined
  text: string
  version: string | null
  editorScroll: ScrollSync
  onChange: (value: string) => void
  onSave: () => void
  onSource: () => void
  onOpenUrl: (url: string) => void
  onOpenFile: (path: string, line?: number) => void
  onScrollLine: (line: number) => void
}): JSX.Element {
  const base = useMemo(() => ({ workspaceId, rootId, directory, hostRoot }), [workspaceId, rootId, directory, hostRoot])
  if (!rich) {
    return (
      <Suspense fallback={null}>
        <MarkdownPreview text={text} version={version} base={base} sync={editorScroll}
          openLocal={(target) => { onOpenFile(target.path, target.line) }}
          onScrollLine={onScrollLine} />
      </Suspense>
    )
  }
  return (
    <Suspense fallback={null}>
      <RichMarkdown path={path} text={text} version={version} base={base}
        onChange={onChange}
        onSave={onSave}
        onSource={onSource}
        onImage={file => saveMarkdownImage((folder, name, bytes) => createBinary(workspaceId, rootId, folder, name, bytes), path, file, getPrefs().markdownImages)}
        onOpenLink={(href) => {
          if (/^(?:https?|mailto):/iu.test(href)) { onOpenUrl(href); return }
          const target = localTarget(href)
          const resolved = target === undefined ? undefined : resolveDocumentPath(directory, target.path)
          if (target !== undefined && resolved !== undefined) onOpenFile(resolved, target.line)
        }} />
    </Suspense>
  )
}
