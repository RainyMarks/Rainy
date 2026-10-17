/** File types the IDE shows in a viewer instead of, or next to, the text editor. */

/** Viewer families chosen by file name: `word` renders .docx in the page, `sheet` spreadsheets, `office` the formats the
 *  installed Office or WPS turns into PDF, `shortcut` Windows .lnk files, and `office-lock` the `~$` owner files Office
 *  writes next to an open document. */
export type IdePreviewKind = 'image' | 'svg' | 'markdown' | 'audio' | 'video' | 'pdf' | 'word' | 'sheet' | 'office' | 'shortcut' | 'office-lock'

/** How one editor tab shows its file; absent means the default for the file type. */
export type IdeTabView = 'source' | 'preview' | 'split' | 'hex'

const IMAGE: Readonly<Record<string, string>> = {
  png: 'image/png', apng: 'image/apng', jpg: 'image/jpeg', jpeg: 'image/jpeg', jfif: 'image/jpeg', gif: 'image/gif', webp: 'image/webp',
  bmp: 'image/bmp', ico: 'image/x-icon', cur: 'image/x-icon', avif: 'image/avif',
}
const AUDIO: Readonly<Record<string, string>> = {
  mp3: 'audio/mpeg', wav: 'audio/wav', ogg: 'audio/ogg', oga: 'audio/ogg', opus: 'audio/ogg', flac: 'audio/flac', m4a: 'audio/mp4',
  aac: 'audio/aac', weba: 'audio/webm',
}
const VIDEO: Readonly<Record<string, string>> = {
  mp4: 'video/mp4', m4v: 'video/mp4', webm: 'video/webm', ogv: 'video/ogg', mov: 'video/quicktime', mkv: 'video/x-matroska',
}
const MARKDOWN = new Set(['md', 'markdown', 'mdown', 'mkd', 'mdx'])
const WORD = new Set(['docx', 'docm', 'dotx', 'dotm'])
const SHEET = new Set(['xlsx', 'xlsm', 'xltx', 'xltm', 'xlsb', 'xls', 'xlt', 'ods'])
/** Slides and legacy Word documents, which need Office or WPS to show faithfully. */
const OFFICE = new Set(['pptx', 'pptm', 'ppsx', 'ppsm', 'potx', 'ppt', 'pps', 'pot', 'odp', 'dps', 'doc', 'dot', 'wps', 'odt'])
/** Extensions an Office owner file (`~$name.docx`) can carry. */
const OFFICE_LOCK = new Set([...WORD, ...SHEET, ...OFFICE, 'rtf'])

function baseName(path: string): string {
  return path.slice(Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\')) + 1)
}

function extension(path: string): string {
  const name = baseName(path)
  const dot = name.lastIndexOf('.')
  return dot <= 0 ? '' : name.slice(dot + 1).toLowerCase()
}

/**
 * @param path File path in any spelling.
 * @returns The viewer family, or `undefined` for files the text editor or hex view shows.
 */
export function idePreviewKind(path: string): IdePreviewKind | undefined {
  const ext = extension(path)
  if (baseName(path).startsWith('~$') && OFFICE_LOCK.has(ext)) return 'office-lock'
  if (ext === 'svg') return 'svg'
  if (MARKDOWN.has(ext)) return 'markdown'
  if (Object.hasOwn(IMAGE, ext)) return 'image'
  if (Object.hasOwn(AUDIO, ext)) return 'audio'
  if (Object.hasOwn(VIDEO, ext)) return 'video'
  if (ext === 'pdf') return 'pdf'
  if (WORD.has(ext)) return 'word'
  if (SHEET.has(ext)) return 'sheet'
  if (OFFICE.has(ext)) return 'office'
  if (ext === 'lnk') return 'shortcut'
  return undefined
}

/**
 * Files whose bytes go to a viewer: the Host skips text decoding and serves them only as a byte stream.
 * @param path File path.
 * @returns Whether the file is anything with a viewer except SVG and Markdown, which are text.
 */
export function ideIsMedia(path: string): boolean {
  const kind = idePreviewKind(path)
  return kind !== undefined && kind !== 'svg' && kind !== 'markdown'
}

/**
 * Response type of the file byte route. Anything not listed is an opaque byte stream.
 * @param path File path.
 * @returns The MIME type.
 */
export function ideMediaType(path: string): string {
  const ext = extension(path)
  if (ext === 'svg') return 'image/svg+xml'
  if (ext === 'pdf') return 'application/pdf'
  return IMAGE[ext] ?? AUDIO[ext] ?? VIDEO[ext] ?? 'application/octet-stream'
}

/** Path of the Host route that streams workspace file bytes to the page. */
export const IDE_RAW_ROUTE = '/rainy/ide/raw'

/**
 * Same-origin URL of a workspace file's bytes. The version makes a changed file a new URL.
 * @param file Project, mounted directory, project-relative path and the observed version.
 * @returns A relative URL for `<img>`, `<video>` or `fetch`.
 */
export function ideRawUrl(file: { workspaceId: string; rootId?: string | undefined; path: string; version?: string | undefined }): string {
  const query = new URLSearchParams({ w: file.workspaceId, p: file.path })
  if (file.rootId !== undefined) query.set('r', file.rootId)
  if (file.version !== undefined) query.set('v', file.version)
  return `${IDE_RAW_ROUTE}?${query.toString()}`
}
