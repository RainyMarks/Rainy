// Host clipboard write shared by Web UI copy controls. Success feedback stays
// with each control; a refused write is announced through the page's notice
// (see onClipboardRefused) unless the caller shows the failure itself.

const refusals = new Set<() => void>()

/**
 * Observe refused writes; most copy controls only show success, so the page
 * says once, here, that a copy failed.
 * @param listener - called after each refused write that was not quiet.
 * @returns a function that removes the listener.
 */
export function onClipboardRefused(listener: () => void): () => void {
  refusals.add(listener)
  return () => { refusals.delete(listener) }
}

/**
 * Write text to the host clipboard, preferring the async Clipboard API and
 * falling back to `execCommand('copy')` on hosts (jsdom, insecure contexts)
 * that omit it.
 * @param text - the exact text to place on the clipboard.
 * @param options - `quiet` when the caller reports a refusal itself.
 * @returns true only when the host accepted the write.
 */
export async function writeClipboard(text: string, options: { quiet?: boolean } = {}): Promise<boolean> {
  const accepted = await write(text)
  if (!accepted && options.quiet !== true) for (const listener of refusals) listener()
  return accepted
}

async function write(text: string): Promise<boolean> {
  // lib.dom types clipboard non-optional, but insecure contexts omit it —
  // that runtime gap is exactly what this guard detects.
  /* oxlint-disable-next-line typescript/no-unnecessary-condition */
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text)
      return true
    } catch {
      // Denied permissions / iframe policy — do not claim success.
      return false
    }
  }
  // jsdom and older hosts: best-effort execCommand path when present.
  // execCommand('copy') is the only clipboard fallback where the async API
  // is missing; deprecated but deliberately retained.
  /* oxlint-disable typescript/no-deprecated */
  const exec = typeof document.execCommand === 'function'
    ? document.execCommand.bind(document)
    : undefined
  if (exec === undefined) return false
  const el = document.createElement('textarea')
  el.value = text
  el.setAttribute('readonly', '')
  el.style.position = 'fixed'
  el.style.left = '-9999px'
  document.body.appendChild(el)
  el.select()
  try {
    return exec('copy')
  } catch {
    return false
  } finally {
    el.remove()
  }
  /* oxlint-enable typescript/no-deprecated */
}
