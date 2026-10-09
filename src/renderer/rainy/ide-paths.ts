/** Renderer-only keys for files whose relative names can repeat across mounted directories. */
import type { IdeFileReference, IdeRootId, IdeWorkspace, IdeWorkspaceRoot } from '../ide-files-protocol.ts'

const prefix = '\0'
const primary = 'primary' as IdeRootId

/** List directory mounts on current or legacy workspace snapshots.
 * @param workspace Selected project.
 * @returns Its primary root and any attached directories.
 */
export function workspaceRoots(workspace: IdeWorkspace): readonly IdeWorkspaceRoot[] {
  return workspace.roots ?? [{ rootId: primary, path: workspace.path, title: workspace.title, primary: true }]
}

/** Build an internal editor key; NUL cannot collide with any filesystem name.
 * @param path Root-relative path.
 * @param rootId Directory identity, omitted for the primary root.
 * @returns Root-qualified editor key.
 */
export function fileKey(path: string, rootId?: IdeRootId): string {
  return rootId === undefined || rootId === primary ? path : `${prefix}${rootId}/${path}`
}

/** Decode a renderer key before it crosses the Host protocol.
 * @param key Editor key.
 * @returns Root identity and portable relative path.
 */
export function fileReference(key: string): IdeFileReference {
  if (!key.startsWith(prefix)) return { path: key }
  const separator = key.indexOf('/')
  if (separator < 2) throw new Error('Invalid editor file identity.')
  return { rootId: key.slice(1, separator) as IdeRootId, path: key.slice(separator + 1) }
}

/** Resolve the mount that owns an editor key.
 * @param workspace Project directory snapshot.
 * @param key Editor key.
 * @returns Its mounted directory, if still attached.
 */
export function fileRoot(workspace: IdeWorkspace, key: string): IdeWorkspaceRoot | undefined {
  const reference = fileReference(key)
  return workspaceRoots(workspace).find(root => root.rootId === (reference.rootId ?? primary))
}

/** Present a root-qualified filename without exposing internal editor keys.
 * @param workspace Selected project.
 * @param key Editor key.
 * @returns Primary-relative path, or root title and relative path for attachments.
 */
export function fileLabel(workspace: IdeWorkspace | null, key: string): string {
  const reference = fileReference(key)
  if (reference.rootId === undefined || workspace === null) return reference.path
  return `${fileRoot(workspace, key)?.title ?? reference.rootId}/${reference.path}`
}

/** Join the Host root and relative path using a normalized URI-compatible spelling.
 * @param workspace Selected project.
 * @param key Editor key.
 * @returns Absolute Host path, or undefined for a removed root.
 */
export function absoluteFilePath(workspace: IdeWorkspace, key: string): string | undefined {
  const root = fileRoot(workspace, key)
  if (root === undefined) return undefined
  return root.path.replaceAll('\\', '/').replace(/\/$/, '') + '/' + fileReference(key).path
}

/** Resolve an absolute Host path to its most specific attached root.
 * @param workspace Selected project.
 * @param path Absolute Host path.
 * @returns Root-qualified file key when contained.
 */
export function keyFromAbsolute(workspace: IdeWorkspace, path: string): string | undefined {
  const candidate = path.replaceAll('\\', '/')
  const windows = /^[A-Za-z]:\//u.test(candidate) || candidate.startsWith('//')
  const compare = (value: string) => windows ? value.toLowerCase() : value
  for (const root of [...workspaceRoots(workspace)].sort((a, b) => b.path.length - a.path.length)) {
    const prefix = root.path.replaceAll('\\', '/').replace(/\/$/, '') + '/'
    if (compare(candidate).startsWith(compare(prefix))) return fileKey(candidate.slice(prefix.length), root.rootId)
  }
  return undefined
}
