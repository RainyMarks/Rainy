/** The text of PDF pages as pdf.js 2.16 reports it, made to copy and search as written: line ends from `hasEOL`, spaces
 *  only between words of space-separated scripts, and Kangxi radicals turned back into the ideographs they stand for. */

/** One item of `page.getTextContent()`; marked-content entries have no `str`. */
export interface PdfTextItem {
  readonly str?: string
  readonly hasEOL?: boolean
  /** `[a, b, c, d, x, y]` in PDF user space; `d` is the font size of upright text. */
  readonly transform?: readonly number[]
  /** Advance of the whole item, in the units of `transform`. */
  readonly width?: number
  readonly height?: number
}

/** Kangxi radicals (U+2F00–U+2FD5) and the CJK radicals supplement (U+2E80–U+2EF3). */
const RADICAL = /[\u2E80-\u2EF3\u2F00-\u2FD5]/gu

/**
 * Chinese fonts draw a Kangxi radical and its ideograph with one glyph, so pdf.js reports ⼆ (U+2F06) where the page shows
 * 二 (U+4E8C). The compatibility mapping (NFKC) of these two blocks gives the ideograph, the mapping
 * `native/crates/app/src/web/pdf_text.rs` applies on export; nothing else is normalized, as NFKC elsewhere would also
 * change full-width forms and ligatures.
 * @param text Text from pdf.js.
 * @returns The text with radicals that have an ideograph replaced by it.
 */
export function fixRadicals(text: string): string {
  return text.replace(RADICAL, radical => radical.normalize('NFKC'))
}

/** Letters and digits of the scripts that separate words with spaces. */
const WORD = /[\p{Script=Latin}\p{Script=Greek}\p{Script=Cyrillic}\p{Nd}]/u

/** Whether two items on one line touch, as the parts of a word split by a change of font do. Items without geometry, or
 *  rotated ones, do not. */
function touching(previous: PdfTextItem, next: PdfTextItem): boolean {
  const a = previous.transform
  const b = next.transform
  if (a === undefined || b === undefined || a.length < 6 || b.length < 6 || previous.width === undefined) return false
  if (a[1] !== 0 || a[2] !== 0 || b[1] !== 0 || b[2] !== 0) return false
  const size = Math.max(Math.abs(a[3]), Math.abs(b[3])) || 1
  if (Math.abs(a[5] - b[5]) > size / 2) return false
  return Math.abs(b[4] - (a[4] + previous.width)) < size * 0.15
}

/**
 * @param items Items of one page in reading order, or the selected part of them.
 * @returns Their text: a line break after every item with `hasEOL`, and a space between two items only where a letter or
 *   digit of a space-separated script meets another with a gap between them, so Chinese runs together and punctuation
 *   stays attached; pdf.js's own spaces are kept.
 */
export function joinTextItems(items: Iterable<PdfTextItem>): string {
  let text = ''
  let previous: PdfTextItem | undefined
  for (const item of items) {
    if (item.str === undefined) continue
    const str = fixRadicals(item.str)
    if (str !== '') {
      if (previous !== undefined && WORD.test(text.at(-1) ?? '') && WORD.test(str[0]) && !touching(previous, item)) text += ' '
      text += str
      previous = item
    }
    if (item.hasEOL === true) {
      text += '\n'
      previous = undefined
    }
  }
  return text.replace(/[ \t]+\n/g, '\n').trimEnd()
}

/** The text layer of one rendered page: pdf.js's `textDivs` and the items they show, index for index. Items with no text
 *  (a bare line end) have a div that is not in the document. */
export interface TextLayerRecord {
  readonly divs: readonly HTMLElement[]
  readonly items: readonly PdfTextItem[]
}

/**
 * What Ctrl+C copies from text layers, in place of the browser's copy of their absolutely placed spans: the selected part of
 * the items joined as `joinTextItems` does, so a selection copies with the spacing and line ends 提取文本 gives.
 * @param range The selection.
 * @param layers Text layers in page order.
 * @returns The selected text, pages on lines of their own, or `undefined` when the selection holds no text-layer text.
 */
export function selectedText(range: Range, layers: readonly TextLayerRecord[]): string | undefined {
  const parts: string[] = []
  for (const layer of layers) {
    let first = -1
    let last = -1
    let from = 0
    let to = 0
    layer.divs.forEach((div, index) => {
      const node = div.firstChild
      if (node === null || node.nodeType !== Node.TEXT_NODE || !div.isConnected || !range.intersectsNode(node)) return
      if (first < 0) {
        first = index
        from = node === range.startContainer ? range.startOffset : 0
      }
      last = index
      to = node === range.endContainer ? range.endOffset : (node as Text).length
    })
    if (first < 0) continue
    const items = layer.items.slice(first, last + 1).map((item, index, all) => {
      let str = item.str ?? ''
      if (index === all.length - 1) str = str.slice(0, to)
      if (index === 0) str = str.slice(from)
      return { ...item, str }
    })
    parts.push(joinTextItems(items))
  }
  return parts.length === 0 ? undefined : parts.join('\n')
}
