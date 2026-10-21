/** Where an image pasted or dropped into a Markdown document is saved, under which name, and how the document refers to it. */
import { fileKey, fileReference } from '../ide-paths.ts'
import { viewerMessages } from './messages.ts'

/** The user's choice: `assets` beside the document, `<name>.assets` (Typora's default), or the document's own folder. */
export type ImageFolder = 'assets' | 'named' | 'same'

/** Largest image saved, in bytes; the Host refuses larger files. */
export const MAX_IMAGE_BYTES = 25 * 1024 * 1024

const EXTENSIONS: Readonly<Record<string, string>> = {
  'image/png': 'png', 'image/jpeg': 'jpg', 'image/gif': 'gif', 'image/webp': 'webp', 'image/svg+xml': 'svg', 'image/bmp': 'bmp',
  'image/avif': 'avif', 'image/x-icon': 'ico', 'image/tiff': 'tif',
}

/**
 * @param document Key of the Markdown file.
 * @param choice Where images go.
 * @returns Key of the folder, and its path relative to the document's folder ('' for the same folder).
 */
export function imageFolder(document: string, choice: ImageFolder): { folder: string; relative: string } {
  const reference = fileReference(document)
  const slash = reference.path.lastIndexOf('/')
  const directory = slash < 0 ? '' : reference.path.slice(0, slash)
  const name = reference.path.slice(slash + 1)
  const relative = choice === 'same' ? '' : choice === 'named' ? `${name.replace(/\.[^.]+$/u, '')}.assets` : 'assets'
  const folder = relative === '' ? directory : directory === '' ? relative : `${directory}/${relative}`
  return { folder: fileKey(folder, reference.rootId), relative }
}

/**
 * A file name for an image: a screenshot from the clipboard (Chromium names it `image.png`) gets a time stamp, as in Typora;
 * a dropped file keeps its name, without characters Windows does not allow.
 * @param file Name and type of the pasted file.
 * @param now Time of the paste.
 * @returns The name.
 */
export function imageFileName(file: { readonly name: string; readonly type: string }, now = new Date()): string {
  const extension = EXTENSIONS[file.type] ?? (/\.([a-z0-9]{1,5})$/iu.exec(file.name)?.[1]?.toLowerCase() ?? 'png')
  const pad = (value: number, length = 2): string => String(value).padStart(length, '0')
  const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}${pad(now.getMilliseconds(), 3)}`
  const stem = file.name.replace(/\.[^.]*$/u, '').replace(/[<>:"/\\|?*\u0000-\u001f]/gu, '-').replace(/[. ]+$/u, '').trim()
  if (stem === '' || /^image$/iu.test(stem)) return `image-${stamp}.${extension}`
  return `${stem}.${extension}`
}

/**
 * @param relative Folder relative to the document, '' for its own folder.
 * @param name File name.
 * @returns The target to write in `![](…)`: spaces and parentheses escaped, other characters (Chinese too) kept readable.
 */
export function markdownImageTarget(relative: string, name: string): string {
  const path = relative === '' ? name : `${relative}/${name}`
  return path.replace(/[ ()<>]/gu, character => `%${character.charCodeAt(0).toString(16).toUpperCase().padStart(2, '0')}`)
}

/**
 * Save an image beside a document and return its Markdown target.
 * @param save Creates the file in the project; returns the key of the file it created, which may carry a `-1` suffix.
 * @param document Key of the Markdown file.
 * @param file The pasted or dropped image.
 * @param choice Where images go.
 * @returns The target to write in the document.
 */
export async function saveMarkdownImage(save: (folder: string, name: string, bytes: Uint8Array) => Promise<string>, document: string, file: File,
  choice: ImageFolder): Promise<string> {
  if (file.size > MAX_IMAGE_BYTES) throw new Error(viewerMessages.t('richImageTooLarge'))
  const { folder, relative } = imageFolder(document, choice)
  const created = await save(folder, imageFileName(file), new Uint8Array(await file.arrayBuffer()))
  const name = fileReference(created).path.split('/').at(-1) ?? created
  return markdownImageTarget(relative, name)
}
