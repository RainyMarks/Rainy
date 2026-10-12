/** Whether the window can be seen; background polling waits while it is minimized or hidden. */

/** @returns Whether the page is hidden. Pages without a document (tests in Node) count as visible. */
export function pageHidden(): boolean {
  return typeof document !== 'undefined' && document.visibilityState === 'hidden'
}

/**
 * @param listener Called each time the page becomes visible again.
 * @returns A function that removes the listener.
 */
export function onPageVisible(listener: () => void): () => void {
  if (typeof document === 'undefined') return () => undefined
  const handle = (): void => { if (!pageHidden()) listener() }
  document.addEventListener('visibilitychange', handle)
  return () => { document.removeEventListener('visibilitychange', handle) }
}
