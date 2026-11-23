/** Find in a PDF (Ctrl+F): the matches of a query in the pages' text, as copying joins it, mapped back to the items of
 *  each page's text layer so they can be marked there. */
import type { LaidOutText } from './pdf-text.ts'

/** Characters `from` to `to` of the string of item `item` of a page's text layer. */
export interface MatchPiece { readonly item: number; readonly from: number; readonly to: number }

/** One match: its page (1-based) and the item pieces it covers. */
export interface PdfMatch { readonly page: number; readonly pieces: readonly MatchPiece[] }

/** More matches than this are not listed; a query that common finds nothing useful. */
export const MAX_MATCHES = 5000
/** Maximum number of laid-out PDF pages retained in the find text LRU cache. */
export const PDF_TEXT_CACHE_MAX_PAGES = 120
/** Maximum total characters retained in the find text LRU cache (~4 MB UTF-16). */
export const PDF_TEXT_CACHE_MAX_CHARS = 2_000_000

/**
 * Bounded LRU cache of laid-out PDF page texts for Ctrl+F search.
 * Enforces both a maximum page count and a maximum character budget so full-text search on huge PDFs cannot grow unboundedly.
 */
export class PdfTextCache {
  private readonly entries = new Map<number, LaidOutText>()
  private totalChars = 0

  constructor(
    readonly maxPages = PDF_TEXT_CACHE_MAX_PAGES,
    readonly maxChars = PDF_TEXT_CACHE_MAX_CHARS,
  ) {}

  get size(): number { return this.entries.size }
  get chars(): number { return this.totalChars }

  /** @returns Cached text for 1-based `page`, promoting it to most-recently-used. */
  get(page: number): LaidOutText | undefined {
    const found = this.entries.get(page)
    if (found === undefined) return undefined
    this.entries.delete(page)
    this.entries.set(page, found)
    return found
  }

  /** Store `value` for 1-based `page`, evicting least-recently-used pages to stay within both limits. */
  set(page: number, value: LaidOutText): void {
    const cost = Math.max(1, value.text.length)
    const previous = this.entries.get(page)
    if (previous !== undefined) {
      this.entries.delete(page)
      this.totalChars -= Math.max(1, previous.text.length)
    }
    // A single page exceeding the entire character budget is left uncached so it does not wipe the rest of the cache.
    if (this.maxPages <= 0 || cost > this.maxChars) return
    while (this.entries.size >= this.maxPages || this.totalChars + cost > this.maxChars) {
      const oldest = this.entries.keys().next()
      if (oldest.done) break
      const evicted = this.entries.get(oldest.value)!
      this.entries.delete(oldest.value)
      this.totalChars -= Math.max(1, evicted.text.length)
    }
    this.entries.set(page, value)
    this.totalChars += cost
  }

  clear(): void {
    this.entries.clear()
    this.totalChars = 0
  }
}

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
 * Append matches from a single 1-based page into `matches`, stopping once `limit` matches are collected.
 * @returns `true` if `limit` was reached and scanning further pages can stop.
 */
export function findMatchesInPage(
  pageNumber: number,
  { text, spans }: LaidOutText,
  pattern: RegExp,
  matches: PdfMatch[],
  limit = MAX_MATCHES,
): boolean {
  if (matches.length >= limit) return true
  let item = 0
  for (const found of text.matchAll(pattern)) {
    if (matches.length >= limit) return true
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
    if (pieces.length > 0) matches.push({ page: pageNumber, pieces })
  }
  return matches.length >= limit
}

/**
 * @param pages The text of every page, in page order.
 * @param pattern From {@link findPattern}.
 * @returns The matches in reading order, at most {@link MAX_MATCHES}.
 */
export function findMatches(pages: readonly LaidOutText[], pattern: RegExp): PdfMatch[] {
  const matches: PdfMatch[] = []
  for (let index = 0; index < pages.length; index++) {
    if (findMatchesInPage(index + 1, pages[index]!, pattern, matches, MAX_MATCHES)) break
  }
  return matches
}
