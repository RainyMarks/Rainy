/** The headings of a Markdown document, for the outline beside the editor and for `#heading` links. */
import { slugify } from '../../chat/Markdown.tsx'
import { splitFrontMatter } from './front-matter.ts'

/** One heading, in document order. */
export interface OutlineHeading {
  /** 1 to 6. */
  readonly level: number
  /** Its text without Markdown syntax. */
  readonly text: string
  /** 1-based line of the file where it starts. */
  readonly line: number
}

/**
 * Remove inline Markdown from a heading's text.
 * @param raw Text after the `#` markers, or the lines of a setext heading.
 * @returns The text a reader sees.
 */
export function plainHeading(raw: string): string {
  return raw
    .replace(/!\[([^\]]*)\]\([^)]*\)/gu, '$1')
    .replace(/\[([^\]]+)\]\([^)]*\)/gu, '$1')
    .replace(/\[([^\]]+)\]\[[^\]]*\]/gu, '$1')
    .replace(/<[^>]+>/gu, '')
    .replace(/\*\*|__|~~|`/gu, '')
    .replace(/(^|[^\p{L}\p{N}\\])[*_](?=\S)|(?<=\S)[*_](?=$|[^\p{L}\p{N}])/gu, '$1')
    .replace(/\\([\\`*_{}[\]()#+\-.!|<>~$])/gu, '$1')
    .replace(/\s+/gu, ' ')
    .trim()
}

/**
 * Find the ATX (`## Title`) and setext (`Title` over `===`) headings, including those in block quotes, outside fenced code,
 * math blocks and front matter: the same headings, in the same order, as the rich editor renders.
 * @param text Document text; CRLF line ends count as one.
 * @returns The headings in document order.
 */
export function markdownOutline(text: string): OutlineHeading[] {
  const markdown = text.replace(/\r\n/gu, '\n')
  const { front } = splitFrontMatter(markdown)
  const lines = markdown.split('\n')
  const headings: OutlineHeading[] = []
  let fence: string | undefined
  let math = false
  /** Consecutive paragraph lines, which a `===` or `---` line turns into a setext heading. */
  let paragraph: { start: number; lines: string[] } | undefined
  for (let index = front === '' ? 0 : front.replace(/\n$/u, '').split('\n').length; index < lines.length; index++) {
    const line = (lines[index] ?? '').replace(/^(?: {0,3}>[ \t]?)+/u, '')
    if (fence !== undefined) {
      const closing = /^ {0,3}(`{3,}|~{3,})[ \t]*$/u.exec(line)?.[1]
      if (closing !== undefined && closing[0] === fence[0] && closing.length >= fence.length) fence = undefined
      continue
    }
    if (math) { if (/^ {0,3}\$\$[ \t]*$/u.test(line)) math = false; continue }
    const opening = /^ {0,3}(`{3,}|~{3,})/u.exec(line)?.[1]
    if (opening !== undefined) { fence = opening; paragraph = undefined; continue }
    if (/^ {0,3}\$\$/u.test(line)) {
      // `$$ x $$` on one line is a whole block; a line that only opens one starts it.
      if (!/^ {0,3}\$\$.*\$\$[ \t]*$/u.test(line) || /^ {0,3}\$\$[ \t]*$/u.test(line)) math = true
      paragraph = undefined
      continue
    }
    const atx = /^ {0,3}(#{1,6})(?:[ \t]+(.*?))?(?:[ \t]+#+)?[ \t]*$/u.exec(line)
    if (atx !== null) {
      headings.push({ level: atx[1]!.length, text: plainHeading(atx[2] ?? ''), line: index + 1 })
      paragraph = undefined
      continue
    }
    const underline = /^ {0,3}(=+|-+)[ \t]*$/u.exec(line)?.[1]
    if (underline !== undefined && paragraph !== undefined) {
      headings.push({ level: underline[0] === '=' ? 1 : 2, text: plainHeading(paragraph.lines.join(' ')), line: paragraph.start + 1 })
      paragraph = undefined
      continue
    }
    // Blank lines, lists, tables, rules and HTML end a paragraph that could become a setext heading.
    if (line.trim() === '' || /^ {0,3}(?:[-*+][ \t]|\d+[.)][ \t]|\||<|[-*_][ \t]*[-*_][ \t]*[-*_])/u.test(line)) { paragraph = undefined; continue }
    if (paragraph === undefined) paragraph = { start: index, lines: [line.trim()] }
    else paragraph.lines.push(line.trim())
  }
  return headings
}

/**
 * Anchors of the headings as GitHub writes them: a repeated slug gets `-1`, `-2`… appended.
 * @param headings Headings in document order.
 * @returns One anchor per heading.
 */
export function headingAnchors(headings: readonly Pick<OutlineHeading, 'text'>[]): string[] {
  const seen = new Map<string, number>()
  return headings.map((heading) => {
    const slug = slugify(heading.text)
    const count = seen.get(slug) ?? 0
    seen.set(slug, count + 1)
    return count === 0 ? slug : `${slug}-${count}`
  })
}

/**
 * @param headings Headings in document order.
 * @param fragment Link target after `#`, possibly percent-encoded.
 * @returns The index of the heading it names, or -1.
 */
export function headingForFragment(headings: readonly Pick<OutlineHeading, 'text'>[], fragment: string): number {
  let wanted = fragment
  try { wanted = decodeURIComponent(fragment) } catch (_malformed) { /* Compare the literal spelling. */ }
  const anchors = headingAnchors(headings)
  const exact = anchors.indexOf(wanted)
  return exact >= 0 ? exact : anchors.indexOf(slugify(wanted))
}
