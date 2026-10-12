/** Editor scroll positions for the split Markdown preview. */

/** Carries the source editor's first visible line to the preview without re-rendering the editor host. */
export class ScrollSync {
  private listener: ((line: number) => void) | undefined
  /** @param listener Receives editor lines. @returns Unsubscribe. */
  subscribe(listener: (line: number) => void): () => void {
    this.listener = listener
    return () => { if (this.listener === listener) this.listener = undefined }
  }
  /** @param line First visible source line, possibly fractional. */
  editorScrolled(line: number): void { this.listener?.(line) }
}
