/** What the outline beside the editor needs from the rich Markdown editor: scrolling to a heading, and which heading is at the
 *  top of its view. The rich editor registers itself while it shows a file; the source editor needs neither. */
import { createStore } from '../ui/store.ts'

/** Scrolls the rich editor showing a file. */
export interface OutlineTarget {
  /** @param index Heading in document order, as {@link markdownOutline} lists them. */
  reveal(index: number): void
}

const targets = new Map<string, OutlineTarget>()

/** The heading at the top of the rich editor's view, by file key; -1 before the first heading. */
export const outlinePosition = createStore<{ readonly path: string | null; readonly index: number }>({ path: null, index: -1 })

/**
 * @param path File key the rich editor shows.
 * @param target How it scrolls to a heading.
 * @returns Withdraws the registration.
 */
export function provideOutline(path: string, target: OutlineTarget): () => void {
  targets.set(path, target)
  return () => {
    if (targets.get(path) === target) targets.delete(path)
    if (outlinePosition.getSnapshot().path === path) outlinePosition.set({ path: null, index: -1 })
  }
}

/** @param path File key. @returns The rich editor showing it, if one is open. */
export function outlineTarget(path: string): OutlineTarget | undefined {
  return targets.get(path)
}
