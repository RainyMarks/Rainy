/** The Host's byte route for project files, which the viewers load images, media, PDF and hex pages from. */

/** Path of the Host route that streams workspace file bytes to the page (`host/src/ide/raw.rs`). */
const IDE_RAW_ROUTE = '/rainy/ide/raw'

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
