/** Find in a PDF (Ctrl+F): the matches of a query in the pages' text, as copying joins it, mapped back to the items of
 *  each page's text layer so they can be marked there. */
import type { LaidOutText } from './pdf-text.ts'

/** Characters `from` to `to` of the string of item `item` of a page's text layer. */
export interface MatchPiece { readonly item: number; readonly from: number; readonly to: number }

/** One match: its page (1-based) and the item pieces it covers. */
export interface PdfMatch { readonly page: number; readonly pieces: readonly MatchPiece[] }

/** More matches than this are not listed; a query that common finds nothing useful. */
export const MAX_MATCHES = 5000

function escape(text: string): string {
  return text.replace(/[\\^$.*+?()[\]{}|]/gu, '\\$&')
}

/**
 * @param query What to find; white space in it matches any white space, also a line break of the page.
 * @param caseSensitive Whether `A` and `a` differ.
 * @returns The expression, or `undefined` for an empty query.
 */
export function findPattern(query: string, caseSensitive: boolean): RegExp | undefined {
  const words = query.trim().split(/\s+/u).filter(word => word !== '')
  if (words.length === 0) return undefined
  return new RegExp(words.map(escape).join('\\s+'), caseSensitive ? 'gu' : 'giu')
}

/**
 * @param pages The text of every page, in page order.
 * @param pattern From {@link findPattern}.
 * @returns The matches in reading order, at most {@link MAX_MATCHES}.
 */
export function findMatches(pages: readonly LaidOutText[], pattern: RegExp): PdfMatch[] {
  const matches: PdfMatch[] = []
  pages.forEach(({ text, spans }, index) => {
    let item = 0
    for (const found of text.matchAll(pattern)) {
      if (matches.length >= MAX_MATCHES) return
      const start = found.index
      const end = start + found[0].length
      if (end === start) continue
      // Matches come in order, so the items before the first one that ends past this match never hold a later one.
      while (item < spans.length && spans[item]![1] <= start) item++
      const pieces: MatchPiece[] = []
      for (let at = item; at < spans.length && spans[at]![0] < end; at++) {
        const [from, to] = spans[at]!
        if (to > start && Math.min(end, to) > Math.max(start, from)) pieces.push({ item: at, from: Math.max(start, from) - from, to: Math.min(end, to) - from })
      }
      if (pieces.length > 0) matches.push({ page: index + 1, pieces })
    }
  })
  return matches
}
