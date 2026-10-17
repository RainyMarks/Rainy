/** Which editor or viewer an editor tab shows, from its file type, the Host's read result and the tab's chosen view. */
import type { IdeFileDocument } from '../../../shared/ide-files-protocol.ts'
import { idePreviewKind, type IdeTabView } from '../../../shared/ide-preview.ts'

/** Encodings offered for reading a file as text when it is not UTF-8. */
export const TEXT_ENCODINGS = ['utf-16le', 'utf-16be', 'gb18030', 'big5', 'shift_jis', 'euc-kr', 'windows-1252', 'utf-8'] as const

/** Viewers other than the text editor. */
export type ViewerKind = 'image' | 'svg' | 'markdown' | 'audio' | 'video' | 'pdf' | 'word' | 'sheet' | 'office' | 'shortcut' | 'office-lock'
  | 'hex' | 'large'

/** What one tab shows: the text editor, a viewer, or both side by side. */
export interface ViewerChoice {
  /** The CodeMirror editor is visible, with the file's text or its decoded text. */
  readonly editor: boolean
  readonly viewer?: ViewerKind | undefined
  /** Editor on the left, viewer on the right. */
  readonly split: boolean
}

/**
 * @param path File key or path; only its extension matters.
 * @param reason Why the Host did not return editable text, or `null` for UTF-8 text.
 * @param view The tab's chosen view; absent means the default for the file type.
 * @param decoded Whether the page decoded the file's text in another encoding.
 * @returns The choice.
 */
export function chooseViewer(path: string, reason: IdeFileDocument['readOnlyReason'], view: IdeTabView | undefined, decoded: boolean): ViewerChoice {
  if (view === 'hex') return { editor: false, viewer: 'hex', split: false }
  const kind = idePreviewKind(path)
  if (reason === 'media') {
    return kind === undefined || kind === 'svg' || kind === 'markdown' ? { editor: false, viewer: 'hex', split: false } : { editor: false, viewer: kind, split: false }
  }
  if (reason === 'binary') return decoded ? { editor: true, split: false } : { editor: false, viewer: 'hex', split: false }
  if (reason === 'unsupported-encoding') return { editor: true, split: false }
  if (reason === 'too-large') return { editor: false, viewer: 'large', split: false }
  if (kind === 'markdown') {
    if (view === 'preview') return { editor: false, viewer: 'markdown', split: false }
    if (view === 'split') return { editor: true, viewer: 'markdown', split: true }
    return { editor: true, split: false }
  }
  if (kind === 'svg') {
    if (view === 'source') return { editor: true, split: false }
    if (view === 'split') return { editor: true, viewer: 'svg', split: true }
    return { editor: false, viewer: 'svg', split: false }
  }
  return { editor: true, split: false }
}

/**
 * Views a tab can switch between, besides hex which every file offers.
 * @param path File key or path.
 * @param reason The Host's read result.
 * @returns Source, preview and split for Markdown and SVG text; nothing otherwise.
 */
export function switchableViews(path: string, reason: IdeFileDocument['readOnlyReason']): readonly IdeTabView[] {
  const kind = idePreviewKind(path)
  return reason === null && (kind === 'markdown' || kind === 'svg') ? ['source', 'preview', 'split'] : []
}

/**
 * The view a tab shows when it has no explicit choice, so the toolbar can mark it.
 * @param path File key or path.
 * @returns `preview` for SVG, `source` for everything else.
 */
export function defaultView(path: string): IdeTabView {
  return idePreviewKind(path) === 'svg' ? 'preview' : 'source'
}
