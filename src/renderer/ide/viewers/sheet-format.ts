/** Pure spreadsheet workbook conversion shared by the Web Worker and the main-thread chunked fallback.
 *  Kept free of dynamic `import()` so Vite's default IIFE worker bundle compiles cleanly without code splitting. */
import type * as SheetJs from 'xlsx'

/** Spreadsheet cells drawn at once; Excel files can be far larger than a useful preview. */
export const MAX_SHEET_ROWS = 2000
export const MAX_SHEET_COLUMNS = 200

/** One sheet as rows of display text, with merged cells. */
export interface SheetGrid {
  readonly name: string
  readonly rows: readonly (readonly string[])[]
  readonly totalRows: number
  readonly totalColumns: number
  /** `row:column` of each merge's top-left cell → its span; covered cells are listed in `hidden`. */
  readonly spans: ReadonlyMap<string, { rows: number; columns: number }>
  readonly hidden: ReadonlySet<string>
}

/** Structured-clone friendly wire shape returned by the sheet Web Worker. */
export interface SerializedSheetGrid {
  readonly name: string
  readonly rows: readonly (readonly string[])[]
  readonly totalRows: number
  readonly totalColumns: number
  readonly spans: readonly (readonly [string, { rows: number; columns: number }])[]
  readonly hidden: readonly string[]
}

/** Excel's built-in short date formats follow the system language, as in Excel; the others are shown as written. */
const SHORT_DATES: Readonly<Record<string, { zh: string; en: string }>> = {
  'm/d/yy': { zh: 'yyyy/m/d', en: 'm/d/yyyy' },
  'm/d/yy h:mm': { zh: 'yyyy/m/d h:mm', en: 'm/d/yyyy h:mm' },
}

const YIELD_CELL_CHUNK = 5000

/**
 * Convert serialized worker output back into {@link SheetGrid} instances with Map/Set lookups.
 * @param items Serialized sheet payloads.
 * @returns Hydrated grids for rendering.
 */
export function hydrateSheetGrids(items: readonly SerializedSheetGrid[]): SheetGrid[] {
  return items.map(item => ({
    name: item.name,
    rows: item.rows,
    totalRows: item.totalRows,
    totalColumns: item.totalColumns,
    spans: new Map(item.spans),
    hidden: new Set(item.hidden),
  }))
}

/**
 * Parse spreadsheet bytes using a supplied SheetJS instance.
 * When `yieldSlice` is provided (fallback path on main thread), yields between sheets and large cell sweeps.
 */
export async function parseWorkbookRaw(
  XLSX: typeof SheetJs,
  bytes: Uint8Array,
  locale: 'zh' | 'en',
  yieldSlice?: () => Promise<void>,
  signal?: AbortSignal,
): Promise<SerializedSheetGrid[]> {
  if (signal?.aborted) throw new DOMException('Aborted', 'AbortError')
  const book = XLSX.read(bytes, { type: 'array', cellNF: true, cellStyles: false, sheetRows: MAX_SHEET_ROWS + 1 })
  const out: SerializedSheetGrid[] = []
  for (const name of book.SheetNames) {
    if (signal?.aborted) throw new DOMException('Aborted', 'AbortError')
    const sheet = book.Sheets[name]
    if (sheet === undefined || sheet['!ref'] === undefined) {
      out.push({ name, rows: [], totalRows: 0, totalColumns: 0, spans: [], hidden: [] })
      continue
    }
    let scanned = 0
    for (const [address, cell] of Object.entries(sheet)) {
      if (address.startsWith('!') || typeof cell !== 'object' || cell === null || !('t' in cell)) continue
      const short = typeof cell.z === 'string' ? SHORT_DATES[cell.z] : undefined
      if (short !== undefined && cell.t === 'n' && typeof cell.v === 'number') cell.w = XLSX.SSF.format(short[locale], cell.v)
      if (yieldSlice !== undefined && ++scanned % YIELD_CELL_CHUNK === 0) {
        await yieldSlice()
        if (signal?.aborted) throw new DOMException('Aborted', 'AbortError')
      }
    }
    const range = XLSX.utils.decode_range(sheet['!fullref'] ?? sheet['!ref'])
    const totalRows = range.e.r + 1
    const totalColumns = range.e.c + 1
    const rows = XLSX.utils.sheet_to_json<string[]>(sheet, { header: 1, raw: false, defval: '', blankrows: true, dateNF: 'yyyy-mm-dd' })
      .slice(0, MAX_SHEET_ROWS).map(row => row.slice(0, MAX_SHEET_COLUMNS).map(String))
    const spans: [string, { rows: number; columns: number }][] = []
    const hidden: string[] = []
    const rowLimit = Math.min(MAX_SHEET_ROWS, rows.length)
    for (const merge of sheet['!merges'] ?? []) {
      if (merge.s.r >= rowLimit || merge.s.c >= MAX_SHEET_COLUMNS) continue
      const endRow = Math.min(merge.e.r, rowLimit - 1)
      const endColumn = Math.min(merge.e.c, MAX_SHEET_COLUMNS - 1)
      if (endRow < merge.s.r || endColumn < merge.s.c) continue
      spans.push([`${merge.s.r}:${merge.s.c}`, { rows: endRow - merge.s.r + 1, columns: endColumn - merge.s.c + 1 }])
      for (let row = merge.s.r; row <= endRow; row++) {
        for (let column = merge.s.c; column <= endColumn; column++) {
          if (row !== merge.s.r || column !== merge.s.c) hidden.push(`${row}:${column}`)
        }
      }
    }
    out.push({ name, rows, totalRows, totalColumns, spans, hidden })
    if (yieldSlice !== undefined) await yieldSlice()
  }
  return out
}
