/** The Host's byte routes for project files and one-shot Office PDF previews. */

/** Path of the Host route that streams workspace file bytes to the page (`host/src/ide/raw.rs`). */
const IDE_RAW_ROUTE = '/rainy/ide/raw'
/** Path of the Host streaming upload route (`host/src/ide/raw.rs`). */
export const IDE_UPLOAD_ROUTE = '/rainy/ide/upload'
/** Path of the Host one-shot route that streams a converted Office PDF (`host/src/ide/office_pdf.rs`). */
export const IDE_OFFICE_PDF_ROUTE = '/rainy/ide/office-pdf'

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

/**
 * Same-origin URL for streaming a file upload into a workspace (`POST /rainy/ide/upload`).
 * @param file Target project, optional mounted directory, project-relative path and overwrite flag.
 * @returns A relative URL on the Host origin.
 */
export function ideUploadUrl(file: { workspaceId: string; rootId?: string | undefined; path: string; overwrite?: boolean | undefined }): string {
  const query = new URLSearchParams({ w: file.workspaceId, p: file.path })
  if (file.rootId !== undefined) query.set('r', file.rootId)
  if (file.overwrite === true) query.set('overwrite', '1')
  return `${IDE_UPLOAD_ROUTE}?${query.toString()}`
}

/**
 * Same-origin URL for consuming a registered one-shot Office PDF token.
 * @param token One-shot token returned by the Host.
 * @returns A relative URL on the Host origin.
 */
export function ideOfficePdfUrl(token: string): string {
  return `${IDE_OFFICE_PDF_ROUTE}?${new URLSearchParams({ token }).toString()}`
}

/**
 * Validate that an Office PDF URL returned across IPC stays strictly on the Host's one-shot route.
 * @param url Relative URL string to validate.
 * @returns Whether `url` is a safe same-origin `/rainy/ide/office-pdf?token=...` URL.
 */
export function isSameOriginOfficePdfUrl(url: string): boolean {
  if (!url.startsWith(`${IDE_OFFICE_PDF_ROUTE}?`)) return false
  if (url.includes('\\') || /[\r\n]/u.test(url)) return false
  try {
    const parsed = new URL(url, 'http://127.0.0.1')
    return parsed.origin === 'http://127.0.0.1' && parsed.pathname === IDE_OFFICE_PDF_ROUTE && (parsed.searchParams.get('token') ?? '') !== ''
  } catch {
    return false
  }
}
