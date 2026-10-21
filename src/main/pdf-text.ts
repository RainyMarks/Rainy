/**
 * Make the text of a PDF that Chromium printed copy and search as written. Chinese fonts such as Microsoft YaHei draw a
 * Kangxi radical and its ideograph (⼆ U+2F06 and 二 U+4E8C) with one glyph, and Chromium's ToUnicode maps then name the
 * radical, so "第二节" copies as "第⼆节" and is not found by search. The maps are rewritten to the ideographs, which
 * Unicode's compatibility mapping (NFKC) gives, as an incremental update appended to the file: nothing already in the file
 * moves. A file this code does not understand is returned unchanged.
 */
import { deflateSync, inflateSync } from 'node:zlib'

/** @returns The ideograph a Kangxi radical or a CJK radical supplement character stands for, or undefined. */
function ideographOf(unit: number): number | undefined {
  if (unit < 0x2e80 || unit > 0x2fdf) return undefined
  const mapped = String.fromCharCode(unit).normalize('NFKC')
  return mapped.length === 1 && mapped.charCodeAt(0) !== unit ? mapped.charCodeAt(0) : undefined
}

const hex4 = (value: number): string => value.toString(16).toUpperCase().padStart(4, '0')

/** @param destination UTF-16BE hex of a mapping's text. @returns It with radicals replaced. */
function fixDestination(destination: string): string {
  if (destination.length % 4 !== 0) return destination
  let fixed = ''
  for (let index = 0; index < destination.length; index += 4) {
    const unit = Number.parseInt(destination.slice(index, index + 4), 16)
    fixed += hex4(ideographOf(unit) ?? unit)
  }
  return fixed
}

/**
 * @param cmap A ToUnicode CMap.
 * @returns The CMap with every destination that names a radical changed to its ideograph, or undefined when none does.
 */
export function fixToUnicode(cmap: string): string | undefined {
  let changed = false
  const fixed = cmap
    .replace(/beginbfchar([\s\S]*?)endbfchar/gu, (_section, body: string) => {
      // Pairs of <source> <destination>; only destinations change.
      const pairs = body.replace(/<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]*)>/gu, (pair, source: string, destination: string) => {
        const next = fixDestination(destination.toUpperCase())
        if (next === destination.toUpperCase()) return pair
        changed = true
        return `<${source}> <${next}>`
      })
      return `beginbfchar${pairs}endbfchar`
    })
    .replace(/beginbfrange([\s\S]*?)endbfrange/gu, (section, body: string) => {
      const ranges = fixRanges(body)
      if (ranges === undefined) return section
      changed = true
      return `beginbfrange${ranges}endbfrange`
    })
  return changed ? fixed : undefined
}

/**
 * @param body The entries of a bfrange section: `<low> <high> <start>` or `<low> <high> [<d1> <d2> …]`.
 * @returns The entries with radicals replaced, a range that reaches one written out as a list; undefined when none does or
 * the section is not understood.
 */
function fixRanges(body: string): string | undefined {
  const tokens = [...body.matchAll(/<([0-9A-Fa-f]*)>|\[|\]/gu)].map(match => match[1] ?? match[0])
  const entries: string[] = []
  let changed = false
  let index = 0
  while (index < tokens.length) {
    const low = tokens[index]
    const high = tokens[index + 1]
    const next = tokens[index + 2]
    if (low === undefined || high === undefined || next === undefined || ['[', ']'].includes(low) || ['[', ']'].includes(high)) return undefined
    if (next === '[') {
      const close = tokens.indexOf(']', index + 3)
      if (close < 0) return undefined
      const list = tokens.slice(index + 3, close)
      const fixed = list.map(destination => fixDestination(destination.toUpperCase()))
      if (fixed.some((value, position) => value !== list[position]!.toUpperCase())) changed = true
      entries.push(`<${low}> <${high}> [${fixed.map(value => `<${value}>`).join(' ')}]`)
      index = close + 1
      continue
    }
    if (next === ']') return undefined
    const count = Number.parseInt(high, 16) - Number.parseInt(low, 16) + 1
    const first = Number.parseInt(next, 16)
    const units = next.length === 4 && count >= 1 && count <= 256 && (first & 0xff) + count <= 0x100
      ? Array.from({ length: count }, (_unused, position) => first + position) : []
    if (units.some(unit => ideographOf(unit) !== undefined)) {
      // Consecutive codes map to consecutive characters; the radicals among them break the run, so each gets its own.
      changed = true
      entries.push(`<${low}> <${high}> [${units.map(unit => `<${hex4(ideographOf(unit) ?? unit)}>`).join(' ')}]`)
    } else entries.push(`<${low}> <${high}> <${next}>`)
    index += 3
  }
  return changed ? `\n${entries.join('\n')}\n` : undefined
}

/**
 * @param pdf A PDF with a classic cross-reference table, as Chromium writes it.
 * @returns The PDF with its ToUnicode maps fixed by an incremental update, or `pdf` itself when nothing needs fixing or
 * the file is not one this code understands.
 */
export function searchablePdf(pdf: Buffer): Buffer {
  try {
    const text = pdf.toString('latin1')
    const tail = /startxref\s+(\d+)\s+%%EOF\s*$/u.exec(text.slice(-64))
    const trailerAt = text.lastIndexOf('trailer')
    if (tail === null || trailerAt < 0) return pdf
    const trailer = text.slice(trailerAt, text.lastIndexOf('startxref'))
    const size = /\/Size\s+(\d+)/u.exec(trailer)?.[1]
    const root = /\/Root\s+(\d+\s+\d+\s+R)/u.exec(trailer)?.[1]
    if (size === undefined || root === undefined || /\/Encrypt\b/u.test(trailer)) return pdf
    const info = /\/Info\s+(\d+\s+\d+\s+R)/u.exec(trailer)?.[1]
    const id = /\/ID\s*(\[[^\]]*\])/u.exec(trailer)?.[1]
    const objects = [...new Set([...text.matchAll(/\/ToUnicode\s+(\d+)\s+0\s+R/gu)].map(match => Number(match[1])))]
    const updates: { readonly id: number; readonly body: Buffer }[] = []
    for (const object of objects) {
      const header = new RegExp(`(?:^|[\\r\\n])${object}\\s+0\\s+obj\\s*<<([^>]*)>>\\s*stream(\\r\\n|\\n)`, 'u').exec(text)
      if (header === null) continue
      const dictionary = header[1]!
      // A direct length only: `/Length 12 0 R` points elsewhere.
      const length = /\/Length\s+(\d+)\s*(?=\/|$)/u.exec(dictionary)?.[1]
      if (!/\/Filter\s*\/FlateDecode/u.test(dictionary) || length === undefined || /\/DecodeParms/u.test(dictionary)) continue
      const start = header.index + header[0].length
      const cmap = inflateSync(pdf.subarray(start, start + Number(length))).toString('latin1')
      const fixed = fixToUnicode(cmap)
      if (fixed === undefined) continue
      const stream = deflateSync(Buffer.from(fixed, 'latin1'))
      updates.push({ id: object, body: Buffer.concat([Buffer.from(`${object} 0 obj\n<</Filter /FlateDecode /Length ${stream.length}>>\nstream\n`, 'latin1'),
        stream, Buffer.from('\nendstream\nendobj\n', 'latin1')]) })
    }
    if (updates.length === 0) return pdf
    const parts: Buffer[] = [pdf]
    let offset = pdf.length
    if (!text.endsWith('\n')) { parts.push(Buffer.from('\n', 'latin1')); offset += 1 }
    // The head of the free list again, as most writers repeat it: some readers take a section that does not start at
    // object 0 for a misnumbered table and renumber its objects.
    let table = 'xref\n0 1\n0000000000 65535 f \n'
    for (const update of updates) {
      parts.push(update.body)
      // Each entry is exactly 20 bytes: offset, generation, kind, space and line feed.
      table += `${update.id} 1\n${String(offset).padStart(10, '0')} 00000 n \n`
      offset += update.body.length
    }
    const trailerText = `trailer\n<</Size ${size} /Root ${root}${info === undefined ? '' : ` /Info ${info}`}${id === undefined ? '' : ` /ID ${id}`} /Prev ${tail[1]}>>\n`
    parts.push(Buffer.from(`${table}${trailerText}startxref\n${offset}\n%%EOF\n`, 'latin1'))
    return Buffer.concat(parts)
  } catch {
    return pdf
  }
}
