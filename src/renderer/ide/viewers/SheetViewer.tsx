/** Excel and OpenDocument spreadsheet viewer with lightweight row virtualization to avoid mounting 400k <td> nodes. */
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { encryptedOpenXml } from '../../../shared/office-protocol.ts'
import { useLocale } from '../../i18n.ts'
import { fileSizeText } from '../../ui/index.ts'
import { loadFailure, type LoadFailure } from './load-errors.ts'
import { useViewerT } from './messages.ts'
import { Encrypted, Failed, useFileBytes, useOpenInApp, type DocumentLocation } from './office-shared.tsx'
import { MAX_SHEET_COLUMNS, MAX_SHEET_ROWS, readSheets, type SheetGrid } from './sheet-parse.ts'
import { ToolActions } from './ToolActions.tsx'
import { errorMessage } from './viewer-utils.ts'
import css from './Viewers.module.css'

/** Marks a spreadsheet that needs a password, in place of an error text. */
const PROTECTED = '\0protected'
/** Sheets with more rows than this render only the visible row window plus overscan. */
const VIRTUAL_ROW_THRESHOLD = 120
const ESTIMATED_ROW_HEIGHT = 24
const OVERSCAN_ROWS = 16

function columnName(index: number): string {
  let name = ''
  for (let value = index + 1; value > 0; value = Math.floor((value - 1) / 26)) name = String.fromCharCode(65 + ((value - 1) % 26)) + name
  return name
}

/**
 * Compute the earliest anchor row that must be included so no `rowSpan` starting above `start` is severed.
 * @param sheet Current sheet grid.
 * @param start Desired first visible row.
 * @returns Adjusted start row <= `start`.
 */
function expandStartForRowSpans(sheet: SheetGrid, start: number): number {
  let earliest = start
  for (const [key, span] of sheet.spans) {
    if (span.rows <= 1) continue
    const row = Number(key.slice(0, key.indexOf(':')))
    if (row < earliest && row + span.rows > start) earliest = row
  }
  return earliest
}

/**
 * Excel and OpenDocument spreadsheets as tables, one tab per sheet.
 * @param props.url Byte route URL.
 * @param props.size File size.
 * @param props.location File name, extension and Host path.
 * @returns The viewer.
 */
export function SheetViewer({ url, size, location }: { url: string; size: number; location: DocumentLocation }): JSX.Element {
  const t = useViewerT()
  const locale = useLocale()
  const latestT = useRef(t)
  latestT.current = t
  const file = useFileBytes(url, size)
  const openInApp = useOpenInApp(location)
  const stageRef = useRef<HTMLDivElement>(null)
  const [sheets, setSheets] = useState<SheetGrid[] | undefined>()
  const [error, setError] = useState<LoadFailure | typeof PROTECTED | undefined>()
  const [active, setActive] = useState(0)
  const [scrollTop, setScrollTop] = useState(0)
  const [viewportHeight, setViewportHeight] = useState(600)

  useEffect(() => {
    let cancelled = false
    const controller = new AbortController()
    setSheets(undefined); setError(undefined); setActive(0)
    if (file.bytes === undefined) return
    if (encryptedOpenXml(file.bytes, location.extension)) { setError(PROTECTED); return }
    void readSheets(file.bytes, locale, controller.signal).then((value) => { if (!cancelled) setSheets(value) },
      (failure: unknown) => {
        if (cancelled || (failure instanceof DOMException && failure.name === 'AbortError')) return
        setError(/password/iu.test(errorMessage(failure)) ? PROTECTED : loadFailure(failure, latestT.current))
      })
    return () => { cancelled = true; controller.abort() }
  }, [file.bytes, location.extension, locale])

  useLayoutEffect(() => {
    const element = stageRef.current
    if (element === null) return
    const measure = (): void => {
      if (element.clientHeight > 0) setViewportHeight(element.clientHeight)
    }
    measure()
    const observer = new ResizeObserver(measure)
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [])

  useEffect(() => {
    setScrollTop(0)
    if (stageRef.current !== null) stageRef.current.scrollTop = 0
  }, [active, sheets])

  const sheet = sheets?.[active]
  const columns = useMemo(() => {
    if (sheet === undefined) return 0
    let maxCols = sheet.totalColumns
    for (const row of sheet.rows) if (row.length > maxCols) maxCols = row.length
    return Math.min(MAX_SHEET_COLUMNS, maxCols)
  }, [sheet])

  const rowCount = sheet?.rows.length ?? 0
  const virtualized = rowCount > VIRTUAL_ROW_THRESHOLD
  const rawStart = virtualized ? Math.max(0, Math.floor(scrollTop / ESTIMATED_ROW_HEIGHT) - OVERSCAN_ROWS) : 0
  const startRow = virtualized && sheet !== undefined ? expandStartForRowSpans(sheet, rawStart) : 0
  const endRow = virtualized
    ? Math.min(rowCount, Math.ceil((scrollTop + viewportHeight) / ESTIMATED_ROW_HEIGHT) + OVERSCAN_ROWS)
    : rowCount
  const topSpacer = startRow * ESTIMATED_ROW_HEIGHT
  const bottomSpacer = Math.max(0, (rowCount - endRow) * ESTIMATED_ROW_HEIGHT)

  const failure = file.error ?? (error === PROTECTED ? undefined : error)
  if (error === PROTECTED) return <div className={css.viewer} data-viewer="sheet"><Encrypted location={location} /></div>
  return <div className={css.viewer} data-viewer="sheet">
    {openInApp !== undefined && <div className={css.toolbar}><span className={css.grow} /><ToolActions actions={[openInApp]} /></div>}
    <div ref={stageRef} className={css.stage} onScroll={(event) => { if (virtualized) setScrollTop(event.currentTarget.scrollTop) }}>
      {failure !== undefined ? <Failed block template={reason => t('sheetFailed', { error: reason })} failure={failure} />
        : sheet === undefined ? <div className={css.message}>{t('loading')}</div>
          : sheet.rows.length === 0 ? <div className={css.message}>{t('sheetEmpty')}</div>
            : <table className={css.sheet}>
              <thead><tr><th />{Array.from({ length: columns }, (_unused, column) => <th key={column}>{columnName(column)}</th>)}</tr></thead>
              <tbody>
                {topSpacer > 0 && <tr aria-hidden="true"><td colSpan={columns + 1} style={{ height: topSpacer, padding: 0, border: 0 }} /></tr>}
                {sheet.rows.slice(startRow, endRow).map((row, offset) => {
                  const rowIndex = startRow + offset
                  return <tr key={rowIndex} data-row={rowIndex}>
                    <th>{rowIndex + 1}</th>
                    {Array.from({ length: columns }, (_unused, column) => {
                      const key = `${rowIndex}:${column}`
                      if (sheet.hidden.has(key)) return null
                      const span = sheet.spans.get(key)
                      const value = row[column] ?? ''
                      return <td key={column} rowSpan={span?.rows} colSpan={span?.columns} data-number={/^-?[\d,.]+%?$/u.test(value) ? '' : undefined}>{value}</td>
                    })}
                  </tr>
                })}
                {bottomSpacer > 0 && <tr aria-hidden="true"><td colSpan={columns + 1} style={{ height: bottomSpacer, padding: 0, border: 0 }} /></tr>}
              </tbody>
            </table>}
    </div>
    {sheets !== undefined && sheets.length > 1 && <div className={css.sheetTabs} role="tablist" aria-label={t('sheetTabs')}>
      {sheets.map((value, index) => <button key={value.name} type="button" role="tab" aria-selected={index === active} className={css.sheetTab} onClick={() => { setActive(index) }}>{value.name}</button>)}
    </div>}
    <div className={css.status}>
      {sheet !== undefined && <span>{t('sheetRows', { rows: sheet.totalRows, columns: sheet.totalColumns })}</span>}
      {sheet !== undefined && (sheet.totalRows > MAX_SHEET_ROWS || sheet.totalColumns > MAX_SHEET_COLUMNS)
        && <span>{t('sheetTruncated', { rows: MAX_SHEET_ROWS, columns: MAX_SHEET_COLUMNS })}</span>}
      <span>{fileSizeText(size)}</span>
    </div>
  </div>
}
