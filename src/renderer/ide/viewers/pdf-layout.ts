/** Geometry of the PDF viewer: how sharp a page canvas may be, and the reader's place in the document, which a change of
 *  zoom keeps. */

/** pdf.js's own `maxCanvasPixels` (16 MP): larger canvases can fail to allocate or exhaust GPU memory. */
export const MAX_CANVAS_PIXELS = 16_777_216

/**
 * @param width CSS width of the page.
 * @param height CSS height of the page.
 * @param ratio `devicePixelRatio`.
 * @param max Most device pixels one canvas may have.
 * @returns Device pixels per CSS pixel to render at: `ratio`, or less when the canvas would exceed `max`, in which case
 *   CSS scales the canvas up and the page is a little softer.
 */
export function canvasPixelRatio(width: number, height: number, ratio: number, max = MAX_CANVAS_PIXELS): number {
  const area = width * height
  if (!(area > 0) || area * ratio * ratio <= max) return ratio
  return Math.sqrt(max / area)
}

/** A page frame, in the content coordinates of the scrolling stage. */
export interface Box {
  readonly top: number
  readonly left: number
  readonly width: number
  readonly height: number
}

/** The visible part of the stage: its scroll position and client size. */
export interface View {
  readonly left: number
  readonly top: number
  readonly width: number
  readonly height: number
}

/** A point of the document to hold still across a zoom: a spot on one page as fractions of the page, the distance past the
 *  page's edge for a point in the margin or a gap (which do not scale), and where in the view it was, as fractions. */
export interface Anchor {
  readonly index: number
  readonly x: number
  readonly y: number
  readonly dx: number
  readonly dy: number
  readonly viewX: number
  readonly viewY: number
}

/**
 * @param count Number of pages.
 * @param top Top of page `index`; pages go down in order.
 * @param y A content coordinate.
 * @returns The index of the last page that starts at or above `y`, or 0.
 */
export function pageIndexAt(count: number, top: (index: number) => number, y: number): number {
  let low = 0
  let high = count - 1
  let found = 0
  while (low <= high) {
    const middle = (low + high) >> 1
    if (top(middle) <= y) {
      found = middle
      low = middle + 1
    } else {
      high = middle - 1
    }
  }
  return found
}

const fraction = (value: number): number => Number.isFinite(value) ? Math.min(1, Math.max(0, value)) : 0

/**
 * @param count Number of pages.
 * @param box Frame of page `index`.
 * @param view The visible part of the stage.
 * @param at The point to hold, as fractions of the view.
 * @returns The anchor of that point, or `undefined` without pages.
 */
export function captureAnchor(count: number, box: (index: number) => Box, view: View, at: { readonly x: number; readonly y: number }): Anchor | undefined {
  if (count === 0) return undefined
  const px = view.left + at.x * view.width
  const py = view.top + at.y * view.height
  const index = pageIndexAt(count, page => box(page).top, py)
  const page = box(index)
  const x = fraction((px - page.left) / page.width)
  const y = fraction((py - page.top) / page.height)
  return { index, x, y, dx: px - (page.left + x * page.width), dy: py - (page.top + y * page.height), viewX: at.x, viewY: at.y }
}

/**
 * @param anchor A point captured before the layout changed.
 * @param count Number of pages now.
 * @param box Frame of page `index` now.
 * @param size Client size of the stage now.
 * @returns The scroll position that puts the point back where it was in the view, or `undefined` when its page is gone.
 */
export function anchoredScroll(anchor: Anchor, count: number, box: (index: number) => Box, size: { readonly width: number; readonly height: number }): { left: number; top: number } | undefined {
  if (anchor.index >= count) return undefined
  const page = box(anchor.index)
  return {
    left: page.left + anchor.x * page.width + anchor.dx - anchor.viewX * size.width,
    top: page.top + anchor.y * page.height + anchor.dy - anchor.viewY * size.height,
  }
}
