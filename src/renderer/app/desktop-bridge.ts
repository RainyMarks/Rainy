/** The desktop carrier's request to save the page's edits before it stops the Host. */

/** A save succeeds only after current edits reach durable Host storage. */
export interface FlushResult {
  readonly ok: boolean
  readonly error?: string
}

/** Desktop saves through the root workbench owner before stopping the Host. */
export interface DesktopWorkbenchBridge {
  /** @param flush - saves current edits. @returns listener disposer. */
  onFlush(flush: () => Promise<FlushResult>): () => void
}
