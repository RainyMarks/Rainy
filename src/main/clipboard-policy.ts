/** Clipboard permission checks for the authenticated desktop Host. */
/**
 * Permit text writes from Host pages while retaining clipboard-read isolation.
 * @param permission - Electron permission name.
 * @param requestingUrl - requesting frame URL.
 * @param origin - authenticated Host origin.
 * @returns whether the permission may be granted.
 */
export function allowsClipboardWrite(permission: string, requestingUrl: string | undefined, origin: string): boolean {
  if (permission !== 'clipboard-sanitized-write' || requestingUrl === undefined) return false
  try { return new URL(requestingUrl).origin === origin }
  catch (_error) { return false } // A malformed requesting URL cannot identify the trusted Host.
}
