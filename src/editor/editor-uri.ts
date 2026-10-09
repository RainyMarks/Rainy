/** Canonical editor identities preserve Host paths while matching Monaco's URI serialization. */
import { URI } from '@codingame/monaco-vscode-api/vscode/vs/base/common/uri'

/** Normalize drive-letter and authority spelling for model creation and filesystem lookup.
 * @param value Source URI or an existing Monaco URI.
 * @returns A URI whose parsed path matches its serialized identity.
 */
export function canonicalEditorUri(value: string | URI): URI {
  const uri = typeof value === 'string' ? URI.parse(value) : value
  return URI.parse(uri.toString())
}

/** Match a source URI to its Host directory without comparing alternative percent encodings.
 * @param value Source URI supplied by the renderer or a language service.
 * @param path Absolute Host root, including native Windows or UNC paths.
 * @returns Whether the file is beneath this root, using the Host's path case semantics.
 */
export function containsEditorUri(value: string, path: string): boolean {
  const uri = canonicalEditorUri(value)
  const root = canonicalEditorUri(URI.file(path))
  if (uri.scheme !== root.scheme || uri.authority !== root.authority) return false
  const prefix = root.path.replace(/\/$/u, '') + '/'
  const windows = /^[A-Za-z]:[\\/]/u.test(path) || path.startsWith('\\\\') || path.startsWith('//')
  return windows ? uri.path.toLowerCase().startsWith(prefix.toLowerCase()) : uri.path.startsWith(prefix)
}
