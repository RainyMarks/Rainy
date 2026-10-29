// @vitest-environment happy-dom
/** The PDF viewer's text extraction, find and copy, its error reasons, the canvas size cap and the place kept across a zoom. */
import { describe, expect, it } from 'vitest'
import { describePdfError, PdfJsUnavailableError } from '../../src/renderer/ide/viewers/pdf-errors.ts'
import { anchoredScroll, canvasPixelRatio, captureAnchor, MAX_CANVAS_PIXELS, pageIndexAt, type Box } from '../../src/renderer/ide/viewers/pdf-layout.ts'
import { findMatches, findPattern } from '../../src/renderer/ide/viewers/pdf-find.ts'
import { fixRadicals, joinTextItems, layOutText, selectedText, type PdfTextItem } from '../../src/renderer/ide/viewers/pdf-text.ts'
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

describe('find', () => {
  const pages = [
    layOutText([item('第一章', 72, 30), item('总则', 102, 20, 700, true), {}, item('Hello', 72, 25, 680), item('world', 100, 25, 680)]),
    layOutText([item('本办法适用于', 72, 60, 700, true), item('hello', 72, 25, 680)]),
  ]

  it('finds text across items, case aside unless asked, and maps it to the text layer', () => {
    const matches = findMatches(pages, findPattern('章总', false)!)
    // `{}` is marked content: it has no div, so the items after it keep their indexes in the text layer.
    expect(matches).toEqual([{ page: 1, pieces: [{ item: 0, from: 2, to: 3 }, { item: 1, from: 0, to: 1 }] }])
    expect(findMatches(pages, findPattern('HELLO', false)!).map(match => match.page)).toEqual([1, 2])
    expect(findMatches(pages, findPattern('HELLO', true)!)).toEqual([])
    expect(findMatches(pages, findPattern('hello', true)!)).toEqual([{ page: 2, pieces: [{ item: 1, from: 0, to: 5 }] }])
  })

  it('matches white space in the query to the spaces and line ends between items', () => {
    expect(findMatches(pages, findPattern('Hello  world', false)!)).toEqual([{ page: 1, pieces: [{ item: 2, from: 0, to: 5 }, { item: 3, from: 0, to: 5 }] }])
    expect(findMatches(pages, findPattern('总则 Hello', false)!)).toHaveLength(1)
  })

  it('takes the query as text and finds nothing for white space alone', () => {
    expect(findPattern('  ', false)).toBeUndefined()
    expect(findMatches([layOutText([{ str: 'a.b (c)' }])], findPattern('.b (c', false)!)).toHaveLength(1)
    expect(findMatches([layOutText([{ str: 'axb' }])], findPattern('.', false)!)).toEqual([])
  })
})

describe('copying a selection of the text layer', () => {
  /** A text layer of the given items, one span per item with text, as pdf.js 2.16 builds it. */
  function layer(items: PdfTextItem[]): { divs: HTMLElement[]; items: PdfTextItem[]; root: HTMLElement } {
    const root = document.createElement('div')
    const divs = items.map((entry) => {
      const span = document.createElement('span')
      span.textContent = entry.str ?? ''
      if (entry.str !== '') root.append(span)
      if (entry.hasEOL === true) root.append(document.createElement('br'))
      return span
    })
    document.body.append(root)
    return { divs, items, root }
  }

  it('copies the selected parts of the items with their line ends, page after page', () => {
    const first = layer([item('Hello', 72, 25), item('world', 100, 25, 700, true), item('', 0, 0, 700, true), item('第二行', 72, 30, 680)])
    const second = layer([item('Next', 72, 20), item('page', 100, 20)])
    const range = document.createRange()
    range.setStart(first.divs[0]!.firstChild!, 2)
    range.setEnd(first.divs[3]!.firstChild!, 2)
    expect(selectedText(range, [first, second])).toBe('llo world\n\n第二')
    range.setEnd(second.divs[1]!.firstChild!, 2)
    expect(selectedText(range, [first, second])).toBe('llo world\n\n第二行\nNext pa')
  })

  it('leaves a selection outside the text layers to the browser', () => {
    const page = layer([item('Hello', 72, 25)])
    const other = document.createElement('p')
    other.textContent = 'toolbar'
    document.body.prepend(other)
    const range = document.createRange()
    range.selectNodeContents(other)
    expect(selectedText(range, [page])).toBeUndefined()
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

describe('canvas size', () => {
  it('renders at the device pixel ratio while the canvas stays under 16 MP', () => {
    expect(canvasPixelRatio(992, 1403, 2.25)).toBe(2.25)
    expect(canvasPixelRatio(0, 0, 2.25)).toBe(2.25)
  })

  it('lowers the ratio of a larger canvas to the cap', () => {
    // An A4 page at 300% on a 225% screen would be about 63 MP.
    const width = 595 * 4 / 3 * 3
    const height = 842 * 4 / 3 * 3
    const ratio = canvasPixelRatio(width, height, 2.25)
    expect(ratio).toBeLessThan(2.25)
    expect(Math.floor(width * ratio) * Math.floor(height * ratio)).toBeLessThanOrEqual(MAX_CANVAS_PIXELS)
    expect(width * height * ratio * ratio).toBeCloseTo(MAX_CANVAS_PIXELS, -2)
  })
})

describe('place across a zoom', () => {
  /** Frames as the stage lays them out: 16 px padding, 12 px gaps, centred in a stage `stage` px wide. */
  function layout(sizes: readonly { width: number; height: number }[], scale: number, stage = 800): Box[] {
    let top = 16
    return sizes.map((size) => {
      const width = size.width * scale
      const height = size.height * scale
      const box = { top, left: Math.max(16, (stage - width) / 2), width, height }
      top += height + 12
      return box
    })
  }
  const sizes = [{ width: 600, height: 800 }, { width: 800, height: 600 }, { width: 600, height: 800 }, { width: 600, height: 300 }]

  it('finds the page at a coordinate whatever the page sizes', () => {
    const boxes = layout(sizes, 1)
    const top = (index: number): number => boxes[index]!.top
    expect(pageIndexAt(boxes.length, top, 0)).toBe(0)
    expect(pageIndexAt(boxes.length, top, 16 + 800 + 5)).toBe(0)
    expect(pageIndexAt(boxes.length, top, 16 + 800 + 12 + 1)).toBe(1)
    expect(pageIndexAt(boxes.length, top, 1e6)).toBe(3)
  })

  it('keeps the same spot of the same page under the same point of the view', () => {
    const before = layout(sizes, 1)
    const view = { left: 0, top: 16 + 800 + 12 + 150, width: 800, height: 600 }
    const anchor = captureAnchor(before.length, index => before[index]!, view, { x: 0.5, y: 0 })!
    expect(anchor).toMatchObject({ index: 1, y: 0.25 })
    const after = layout(sizes, 2)
    const scroll = anchoredScroll(anchor, after.length, index => after[index]!, { width: 800, height: 600 })!
    expect(scroll.top).toBeCloseTo(after[1]!.top + 0.25 * after[1]!.height)
    const again = captureAnchor(after.length, index => after[index]!, { ...view, ...scroll }, { x: 0.5, y: 0 })!
    expect(again.index).toBe(1)
    expect(again.y).toBeCloseTo(0.25)
    expect(again.x).toBeCloseTo(anchor.x)
  })

  it('holds a pointer position, and a point in a gap stays in the gap', () => {
    const before = layout(sizes, 1)
    const view = { left: 0, top: 1000, width: 800, height: 600 }
    const at = { x: 0.2, y: 0.4 }
    const anchor = captureAnchor(before.length, index => before[index]!, view, at)!
    const after = layout(sizes, 1.5)
    const scroll = anchoredScroll(anchor, after.length, index => after[index]!, view)!
    const again = captureAnchor(after.length, index => after[index]!, { ...view, ...scroll }, at)!
    expect([again.index, again.x, again.y]).toEqual([anchor.index, expect.closeTo(anchor.x), expect.closeTo(anchor.y)])
    // 6 px into the gap below the first page.
    const gap = captureAnchor(before.length, index => before[index]!, { ...view, top: 16 + 800 + 6 }, { x: 0.5, y: 0 })!
    expect(gap).toMatchObject({ index: 0, y: 1, dy: 6 })
    expect(anchoredScroll(gap, after.length, index => after[index]!, view)!.top).toBeCloseTo(16 + 800 * 1.5 + 6)
  })

  it('has nothing to hold without pages, or when the page is gone', () => {
    expect(captureAnchor(0, () => ({ top: 0, left: 0, width: 0, height: 0 }), { left: 0, top: 0, width: 1, height: 1 }, { x: 0, y: 0 })).toBeUndefined()
    const anchor = { index: 5, x: 0, y: 0, dx: 0, dy: 0, viewX: 0, viewY: 0 }
    expect(anchoredScroll(anchor, 2, () => ({ top: 0, left: 0, width: 1, height: 1 }), { width: 1, height: 1 })).toBeUndefined()
  })
})
