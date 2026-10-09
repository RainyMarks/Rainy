/** File URI comparison across the spellings the IDE and language servers produce for the same Host path. */

/**
 * Comparison key for a document URI. Language servers may encode a Windows drive colon (`c%3A`) or change its case,
 * and Windows paths compare case-insensitively.
 * @param value URI from the IDE or a language server.
 * @returns A key equal for every spelling of the same file; non-file URIs are returned unchanged.
 */
export function uriKey(value: string): string {
  let url: URL
  try { url = new URL(value) } catch (_error) { return value } // Snippet and recovery ids are not URLs.
  if (url.protocol !== 'file:') return value
  let path: string
  try { path = decodeURIComponent(url.pathname) } catch (_error) { path = url.pathname } // A malformed escape keeps the raw path.
  const windows = /^\/[A-Za-z]:/.test(path) || url.host !== ''
  return `file://${url.host.toLowerCase()}${windows ? path.toLowerCase() : path}`
}

/**
 * Whether a document URI lies under a Host directory.
 * @param value Document URI.
 * @param root Absolute Host directory: POSIX, `C:/…` or UNC.
 * @returns Whether the file is inside the directory.
 */
export function uriInside(value: string, root: string): boolean {
  const key = uriKey(value)
  const normalized = root.replace(/\\/g, '/').replace(/\/$/, '')
  const rootUri = /^[A-Za-z]:\//.test(normalized) ? `file:///${normalized}` : normalized.startsWith('//') ? `file:${normalized}` : `file://${normalized}`
  return key.startsWith(`${uriKey(rootUri)}/`)
}

/**
 * File URI for a Host directory.
 * @param root Absolute Host directory.
 * @returns The `file:` URI.
 */
export function directoryUri(root: string): string {
  const normalized = root.replace(/\\/g, '/').replace(/\/$/, '')
  const drive = /^[A-Za-z]:\//.test(normalized)
  const prefix = drive ? 'file:///' : normalized.startsWith('//') ? 'file:' : 'file://'
  return prefix + normalized.split('/').map((part, index) => drive && index === 0 ? part : encodeURIComponent(part)).join('/')
}
