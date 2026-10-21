/** Exported PDFs copy and search Chinese as written, not as the Kangxi radicals Chromium's ToUnicode maps name. */
import { deflateSync, inflateSync } from 'node:zlib'
import { describe, expect, it } from 'vitest'
import { fixToUnicode, searchablePdf } from '../../src/main/pdf-text.ts'

const cmap = (entries: string): string =>
  `/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n${entries}endcmap\nend\nend\n`

describe('ToUnicode maps', () => {
  it('names the ideograph instead of the radical, leaving sources and other characters alone', () => {
    // ⼆ U+2F06 is 二 U+4E8C and ⽂ U+2F42 is 文 U+6587; a two-byte source code may look like a radical too.
    const fixed = fixToUnicode(cmap('3 beginbfchar\n<E6> <2F06>\n<2F42> <2F42>\n<4E> <5BFC>\nendbfchar\n'))
    expect(fixed).toContain('<E6> <4E8C>')
    expect(fixed).toContain('<2F42> <6587>')
    expect(fixed).toContain('<4E> <5BFC>')
    expect(fixToUnicode(cmap('1 beginbfchar\n<4E> <5BFC>\nendbfchar\n'))).toBeUndefined()
  })

  it('writes out a range that reaches a radical and fixes lists, keeping other ranges', () => {
    const fixed = fixToUnicode(cmap('3 beginbfrange\n<01> <03> <2F05>\n<10> <11> <0041>\n<20> <21> [<2F06> <0042>]\nendbfrange\n'))
    // ⼅ U+2F05 is 亅 U+4E85 and ⼇ U+2F07 is 亠 U+4EA0.
    expect(fixed).toContain('<01> <03> [<4E85> <4E8C> <4EA0>]')
    expect(fixed).toContain('<10> <11> <0041>')
    expect(fixed).toContain('<20> <21> [<4E8C> <0042>]')
    expect(fixToUnicode(cmap('1 beginbfrange\n<10> <11> <0041>\nendbfrange\n'))).toBeUndefined()
  })
})

/** A one-font PDF with a classic cross-reference table, as Chromium writes them. */
function samplePdf(map: string): Buffer {
  const stream = deflateSync(Buffer.from(map, 'latin1'))
  const objects = [
    Buffer.from('<</Type /Catalog /Pages 2 0 R>>'),
    Buffer.from('<</Type /Pages /Kids [] /Count 0>>'),
    Buffer.from('<</Type /Font /Subtype /Type3 /ToUnicode 4 0 R>>'),
    Buffer.concat([Buffer.from(`<</Filter /FlateDecode /Length ${stream.length}>> stream\n`), stream, Buffer.from('\nendstream')]),
  ]
  const parts: Buffer[] = [Buffer.from('%PDF-1.4\n')]
  const offsets: number[] = []
  let offset = parts[0]!.length
  objects.forEach((body, index) => {
    const object = Buffer.concat([Buffer.from(`${index + 1} 0 obj\n`), body, Buffer.from('\nendobj\n')])
    offsets.push(offset)
    parts.push(object)
    offset += object.length
  })
  const table = `xref\n0 5\n0000000000 65535 f \n${offsets.map(value => `${String(value).padStart(10, '0')} 00000 n \n`).join('')}`
  parts.push(Buffer.from(`${table}trailer\n<</Size 5\n/Root 1 0 R>>\nstartxref\n${offset}\n%%EOF`))
  return Buffer.concat(parts)
}

describe('exported PDFs', () => {
  it('appends the fixed maps as an incremental update that points back at the original table', () => {
    const original = samplePdf(cmap('1 beginbfchar\n<E6> <2F06>\nendbfchar\n'))
    const fixed = searchablePdf(original)
    expect(fixed.subarray(0, original.length).equals(original)).toBe(true)
    const update = fixed.subarray(original.length).toString('latin1')
    const previous = /startxref\s+(\d+)\s+%%EOF$/u.exec(original.toString('latin1'))![1]
    expect(update).toMatch(new RegExp(`/Size 5 /Root 1 0 R /Prev ${previous}>>`, 'u'))
    // The new table's entry for object 4 points at the new object, whose stream holds the ideograph.
    const entry = /xref\n0 1\n0000000000 65535 f \n4 1\n(\d{10}) 00000 n \n/u.exec(update)
    expect(entry).not.toBeNull()
    const at = Number(entry![1])
    expect(fixed.subarray(at, at + 8).toString('latin1')).toBe('4 0 obj\n')
    const startxref = Number(/startxref\n(\d+)\n%%EOF\n$/u.exec(update)![1])
    expect(fixed.subarray(startxref, startxref + 4).toString('latin1')).toBe('xref')
    const header = /<<\/Filter \/FlateDecode \/Length (\d+)>>\nstream\n/u.exec(fixed.subarray(at).toString('latin1'))!
    const begin = at + header.index + header[0].length
    expect(inflateSync(fixed.subarray(begin, begin + Number(header[1]))).toString('latin1')).toContain('<E6> <4E8C>')
  })

  it('returns a PDF unchanged when no map names a radical, and anything it does not understand', () => {
    const plain = samplePdf(cmap('1 beginbfchar\n<4E> <5BFC>\nendbfchar\n'))
    expect(searchablePdf(plain)).toBe(plain)
    const garbage = Buffer.from('not a pdf')
    expect(searchablePdf(garbage)).toBe(garbage)
    const truncated = samplePdf(cmap('1 beginbfchar\n<E6> <2F06>\nendbfchar\n')).subarray(0, 200)
    expect(searchablePdf(truncated)).toBe(truncated)
  })
})
