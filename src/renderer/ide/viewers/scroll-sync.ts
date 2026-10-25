/** Editor scroll positions for the split Markdown preview. */

/** Carries the source editor's first visible line to the preview without re-rendering the editor host. */
export class ScrollSync {
  private listener: ((line: number) => void) | undefined
  private last: number | undefined
  /** @param listener Receives editor lines, first the latest one sent before it subscribed. @returns Unsubscribe. */
  subscribe(listener: (line: number) => void): () => void {
    this.listener = listener
    if (this.last !== undefined) listener(this.last)
    return () => { if (this.listener === listener) this.listener = undefined }
  }
  /** @param line First visible source line, 1-based, possibly fractional. */
  editorScrolled(line: number): void {
    this.last = line
    this.listener?.(line)
  }
}
