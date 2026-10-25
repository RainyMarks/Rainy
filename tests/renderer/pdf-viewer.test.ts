// @vitest-environment happy-dom
/** The PDF viewer's text extraction and its error reasons. */
import { describe, expect, it } from 'vitest'
import { describePdfError, PdfJsUnavailableError } from '../../src/renderer/ide/viewers/pdf-errors.ts'
import { fixRadicals, joinTextItems, type PdfTextItem } from '../../src/renderer/ide/viewers/pdf-text.ts'
import radicals from '../../native/crates/app/src/web/radicals.in?raw'

/** An upright item at `x`, `y` in a 10 pt font, `width` wide. */
const item = (str: string, x: number, width: number, y = 700, hasEOL = false): PdfTextItem => ({ str, hasEOL, transform: [10, 0, 0, 10, x, y], width, height: 10 })

describe('radicals', () => {
  it('turns Kangxi and supplement radicals into their ideographs, as the export does', () => {
    expect(fixRadicals('第⼆节')).toBe('第二节')
    expect(fixRadicals('⼀⼈⼤⽇')).toBe('一人大日')
    expect(fixRadicals('⺟⻳')).toBe('母龟')
    // A radical without a compatibility ideograph stays, and nothing outside the two blocks is normalized.
    expect(fixRadicals('⺀')).toBe('⺀')
    expect(fixRadicals('Ａ１ﬁ①')).toBe('Ａ１ﬁ①')
  })

  it('maps exactly the pairs of native/crates/app/src/web/radicals.in', () => {
    const pairs = [...radicals.matchAll(/\(0x([0-9A-F]+), 0x([0-9A-F]+)\)/g)].map(match => [Number.parseInt(match[1]!, 16), Number.parseInt(match[2]!, 16)] as const)
    expect(pairs.length).toBeGreaterThan(200)
    const native = new Map(pairs)
    for (const ranges of [[0x2E80, 0x2EF3], [0x2F00, 0x2FD5]] as const) {
      for (let code = ranges[0]; code <= ranges[1]; code += 1) {
        const char = String.fromCodePoint(code)
        expect(fixRadicals(char)).toBe(String.fromCodePoint(native.get(code) ?? code))
      }
    }
  })
})

describe('text of a page', () => {
  it('runs Chinese together and breaks lines where pdf.js reports a line end', () => {
    const items = [item('第一章', 72, 30), item('总则', 102, 20, 700, true), item('', 0, 0, 700, true), item('本办法', 72, 30, 680), item('适用于', 102, 30, 680)]
    expect(joinTextItems(items)).toBe('第一章总则\n\n本办法适用于')
  })

  it('puts a space between Latin words with a gap, not inside a word split by a font change', () => {
    expect(joinTextItems([item('Hello', 72, 25), item('world', 100, 25)])).toBe('Hello world')
    // "Hel" ends where "lo" starts: one word in two fonts.
    expect(joinTextItems([item('Hel', 72, 15), item('lo', 87, 10)])).toBe('Hello')
    // Items without geometry get the space.
    expect(joinTextItems([{ str: 'GPT' }, { str: '4' }])).toBe('GPT 4')
  })

  it('adds no space before punctuation, between CJK and Latin, or next to a space pdf.js already put in', () => {
    expect(joinTextItems([item('Fig', 72, 15), item('.', 100, 3), item(' ', 103, 3), item('3', 106, 5)])).toBe('Fig. 3')
    expect(joinTextItems([item('使用', 72, 20), item('Python', 100, 30), item('编程', 140, 20)])).toBe('使用Python编程')
    expect(joinTextItems([item('a ', 72, 10), item('b', 100, 5)])).toBe('a b')
  })

  it('maps radicals, skips marked content and trims line ends', () => {
    expect(joinTextItems([{ str: '第⼆节   ', hasEOL: true }, {}, { str: '⽇期' }])).toBe('第二节\n日期')
  })
})

describe('error reasons', () => {
  const named = (name: string, message: string, extra: Record<string, unknown> = {}): unknown => Object.assign(new Error(message), { name }, extra)

  it('chooses a reason by the name pdf.js gives its errors and keeps the message as the detail', () => {
    expect(describePdfError(named('InvalidPDFException', 'Invalid PDF structure.'))).toEqual({ key: 'pdfErrorInvalid', detail: 'Invalid PDF structure.', detailed: false })
    expect(describePdfError(named('MissingPDFException', 'Missing PDF "x".')).key).toBe('pdfErrorMissing')
    expect(describePdfError(named('PasswordException', 'No password given', { code: 1 })).key).toBe('pdfErrorPassword')
    expect(describePdfError(named('FormatError', 'Bad FCHECK in flate stream')).key).toBe('pdfErrorDamaged')
    expect(describePdfError(named('UnknownErrorException', 'Bad XRef entry'))).toMatchObject({ key: 'pdfErrorDamaged', detailed: true })
    expect(describePdfError(new PdfJsUnavailableError('pdf.js is unavailable')).key).toBe('pdfErrorLibrary')
    expect(describePdfError(new Error('Setting up fake worker failed: "Failed to fetch".')).key).toBe('pdfErrorLibrary')
    expect(describePdfError(new TypeError('Failed to fetch')).key).toBe('pdfErrorNetwork')
  })

  it('tells the HTTP answers of the byte route apart', () => {
    const response = (status: number): unknown => named('UnexpectedResponseException', `Unexpected server response (${status}) while retrieving PDF "x".`, { status })
    expect(describePdfError(response(404)).key).toBe('pdfErrorMissing')
    expect(describePdfError(response(409)).key).toBe('pdfErrorChanged')
    expect(describePdfError(response(503)).key).toBe('pdfErrorBusy')
    expect(describePdfError(response(0)).key).toBe('pdfErrorNetwork')
    expect(describePdfError(response(500))).toMatchObject({ key: 'pdfErrorHttp', vars: { status: 500 } })
  })

  it('falls back to a general reason with the detail shown', () => {
    expect(describePdfError(new Error('Something odd'))).toEqual({ key: 'pdfErrorUnknown', detail: 'Something odd', detailed: true })
    expect(describePdfError('plain text').detail).toBe('plain text')
  })
})
