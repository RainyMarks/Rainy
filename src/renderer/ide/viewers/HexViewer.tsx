/** Hex view of a file of any size: virtual rows over range reads, selection, offsets, inspector, copy formats and a binwalk-style scan. */
import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type MouseEvent } from 'react'
import { fileSizeText, isComposingKey, toast, writeClipboard } from '../../ui/index.ts'
import { ByteSource, FileChangedError } from './byte-source.ts'
import { asciiByte, formatBytes, formatOffset, hexByte, HEX_ROW_BYTES, inspectBytes, parseOffset, type CopyFormat } from './hex-format.ts'
import { useViewerT } from './messages.ts'
import { detectSignature, EMBEDDED_OVERLAP, pngEnd, scanChunk, type FileSignature } from './signature.ts'
import { TEXT_ENCODINGS } from './viewer-choice.ts'
import css from './Viewers.module.css'

const ROW_HEIGHT = 20
/** Browsers cap element heights near 33 million pixels; taller files scroll through a scaled spacer. */
const MAX_SPACER = 8_000_000
const MAX_COPY = 1024 * 1024
const MAX_SCAN_RESULTS = 500
const COLUMNS = Array.from({ length: HEX_ROW_BYTES }, (_unused, index) => index)

interface Selection { readonly anchor: number; readonly focus: number }

/**
 * @param props.url Byte route URL of one observed version.
 * @param props.size File size of that version.
 * @param props.onChanged The file changed on disk; the owner reloads it.
 * @param props.onOpenText Read the file as text in an encoding.
 * @returns The viewer.
 */
export function HexViewer({ url, size, onChanged, onOpenText }: {
  url: string
  size: number
  onChanged: () => void
  onOpenText?: ((encoding: string) => void) | undefined
}): JSX.Element {
  const t = useViewerT()
  const source = useMemo(() => new ByteSource(url, size), [url, size])
  const scroller = useRef<HTMLDivElement>(null)
  const gotoInput = useRef<HTMLInputElement>(null)
  const dragging = useRef(false)
  const [, setLoaded] = useState(0)
  const [scrollTop, setScrollTop] = useState(0)
  const [height, setHeight] = useState(400)
  const [selection, setSelection] = useState<Selection | undefined>()
  const [inspected, setInspected] = useState<Uint8Array | undefined>()
  const [littleEndian, setLittleEndian] = useState(true)
  const [signature, setSignature] = useState<FileSignature | null | undefined>()
  const [trailing, setTrailing] = useState<{ format: string; offset: number } | undefined>()
  const [scan, setScan] = useState<{ percent: number; results: readonly { offset: number; label: string }[]; done: boolean } | undefined>()
  const scanning = useRef<AbortController | undefined>(undefined)
  const [error, setError] = useState('')

  const fail = (failure: unknown): void => {
    if (failure instanceof FileChangedError) { onChanged(); return }
    setError(failure instanceof Error ? failure.message : String(failure))
  }

  useEffect(() => {
    setSelection(undefined); setSignature(undefined); setTrailing(undefined); setError('')
    scanning.current?.abort()
    setScan(undefined)
    let cancelled = false
    void source.read(0, 4096).then(async (head) => {
      if (cancelled) return
      const detected = detectSignature(head) ?? null
      setSignature(detected)
      if (detected?.label === 'PNG image') {
        const end = await pngEnd((offset, length) => source.read(offset, length), size)
        if (!cancelled && end !== undefined && end < size) setTrailing({ format: 'PNG', offset: end })
      }
    }).catch((failure: unknown) => { if (!cancelled) fail(failure) })
    return () => { cancelled = true; scanning.current?.abort() }
  }, [source])

  useEffect(() => {
    const element = scroller.current
    if (element === null) return
    const observer = new ResizeObserver(() => { setHeight(element.clientHeight) })
    observer.observe(element)
    return () => { observer.disconnect() }
  }, [])

  const rows = Math.max(1, Math.ceil(size / HEX_ROW_BYTES))
  const total = rows * ROW_HEIGHT
  const scale = total > MAX_SPACER ? total / MAX_SPACER : 1
  const firstRow = Math.min(rows - 1, Math.max(0, Math.floor(scrollTop * scale / ROW_HEIGHT)))
  const lastRow = Math.min(rows, firstRow + Math.ceil(height / ROW_HEIGHT) + 2)

  useEffect(() => {
    let cancelled = false
    void source.load(firstRow * HEX_ROW_BYTES, (lastRow - firstRow) * HEX_ROW_BYTES)
      .then(() => { if (!cancelled) setLoaded(value => value + 1) }, (failure: unknown) => { if (!cancelled) fail(failure) })
    return () => { cancelled = true }
  }, [source, firstRow, lastRow])

  const start = selection === undefined ? undefined : Math.min(selection.anchor, selection.focus)
  const end = selection === undefined ? undefined : Math.max(selection.anchor, selection.focus)
  useEffect(() => {
    if (start === undefined) { setInspected(undefined); return }
    let cancelled = false
    void source.read(start, 8).then((bytes) => { if (!cancelled) setInspected(bytes) }, (failure: unknown) => { if (!cancelled) fail(failure) })
    return () => { cancelled = true }
  }, [source, start])

  const reveal = (offset: number): void => {
    const element = scroller.current
    if (element === null) return
    const row = Math.floor(offset / HEX_ROW_BYTES)
    const visibleRows = Math.max(1, Math.floor(height / ROW_HEIGHT) - 1)
    if (row >= firstRow && row < firstRow + visibleRows) return
    element.scrollTop = Math.max(0, (row - Math.floor(visibleRows / 3)) * ROW_HEIGHT / scale)
  }
  const select = (anchor: number, focus = anchor): void => {
    const clamp = (value: number): number => Math.min(Math.max(0, size - 1), Math.max(0, value))
    setSelection({ anchor: clamp(anchor), focus: clamp(focus) })
    reveal(clamp(focus))
  }

  const offsetAt = (event: MouseEvent<HTMLElement>): number | undefined => {
    const value = (event.target as HTMLElement).closest<HTMLElement>('[data-offset]')?.dataset.offset
    return value === undefined ? undefined : Number(value)
  }
  const onMouseDown = (event: MouseEvent<HTMLDivElement>): void => {
    const offset = offsetAt(event)
    if (offset === undefined || event.button !== 0) return
    event.preventDefault()
    scroller.current?.focus()
    dragging.current = true
    setSelection(previous => ({ anchor: event.shiftKey && previous !== undefined ? previous.anchor : offset, focus: offset }))
  }
  const onMouseMove = (event: MouseEvent<HTMLDivElement>): void => {
    if (!dragging.current || (event.buttons & 1) === 0) { dragging.current = false; return }
    const offset = offsetAt(event)
    if (offset !== undefined) setSelection(previous => previous === undefined ? undefined : { anchor: previous.anchor, focus: offset })
  }

  const copy = async (format: CopyFormat): Promise<void> => {
    if (start === undefined || end === undefined) return
    if (end - start + 1 > MAX_COPY) { toast(t('hexCopyLimit'), { tone: 'error' }); return }
    try {
      if (await writeClipboard(formatBytes(await source.read(start, end - start + 1), format), { quiet: true })) toast(t('hexCopied'), { tone: 'success' })
      else toast(t('copyFailed'), { tone: 'error' })
    } catch (failure) { fail(failure) }
  }

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    const page = Math.max(1, Math.floor(height / ROW_HEIGHT) - 1) * HEX_ROW_BYTES
    const focus = selection?.focus ?? 0
    const moves: Record<string, number | undefined> = {
      ArrowLeft: focus - 1, ArrowRight: focus + 1, ArrowUp: focus - HEX_ROW_BYTES, ArrowDown: focus + HEX_ROW_BYTES,
      PageUp: focus - page, PageDown: focus + page,
      Home: event.ctrlKey ? 0 : focus - (focus % HEX_ROW_BYTES), End: event.ctrlKey ? size - 1 : focus - (focus % HEX_ROW_BYTES) + HEX_ROW_BYTES - 1,
    }
    if (event.key in moves && size > 0) {
      event.preventDefault()
      const next = moves[event.key]!
      select(event.shiftKey && selection !== undefined ? selection.anchor : next, next)
      return
    }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'c') { event.preventDefault(); void copy('hex'); return }
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'g') { event.preventDefault(); gotoInput.current?.focus(); gotoInput.current?.select(); return }
    if (event.key === 'Escape') setSelection(undefined)
  }

  const goto = (text: string): void => {
    const offset = parseOffset(text)
    if (offset === undefined || offset >= Math.max(1, size)) { toast(t('hexGotoInvalid'), { tone: 'error' }); return }
    select(offset)
    scroller.current?.focus()
  }

  const runScan = async (): Promise<void> => {
    scanning.current?.abort()
    const controller = new AbortController()
    scanning.current = controller
    const results: { offset: number; label: string }[] = []
    setScan({ percent: 0, results, done: false })
    try {
      const response = await fetch(url, { signal: controller.signal, cache: 'no-store' })
      if (!response.ok || response.body === null) throw new Error(`HTTP ${response.status}`)
      const reader = response.body.getReader()
      let carried = new Uint8Array(0)
      let received = 0
      for (;;) {
        const { value, done } = await reader.read()
        const block = new Uint8Array(carried.length + (value?.length ?? 0))
        block.set(carried)
        if (value !== undefined) block.set(value, carried.length)
        const base = received - carried.length
        received += value?.length ?? 0
        for (const match of scanChunk(block, base, done)) if (results.length < MAX_SCAN_RESULTS) results.push(match)
        carried = done ? new Uint8Array(0) : block.slice(Math.max(0, block.length - EMBEDDED_OVERLAP))
        if (done) break
        setScan({ percent: size === 0 ? 100 : Math.floor(received / size * 100), results: [...results], done: false })
      }
      setScan({ percent: 100, results: [...results], done: true })
    } catch (failure) {
      if (!controller.signal.aborted) fail(failure)
      setScan(previous => previous === undefined ? undefined : { ...previous, done: true })
    }
  }

  const rowsView: JSX.Element[] = []
  for (let row = firstRow; row < lastRow; row++) {
    const rowStart = row * HEX_ROW_BYTES
    const hex: JSX.Element[] = []
    const text: JSX.Element[] = []
    for (const column of COLUMNS) {
      const offset = rowStart + column
      if (offset >= size) {
        hex.push(<span key={column} className={css.hexByte}>{'  '}</span>)
        text.push(<span key={column} className={css.hexChar}> </span>)
        continue
      }
      const byte = source.byte(offset)
      const selected = start !== undefined && end !== undefined && offset >= start && offset <= end ? true : undefined
      const cursor = selection?.focus === offset || undefined
      hex.push(<span key={column} className={css.hexByte} data-offset={offset} data-selected={selected} data-cursor={cursor}
        data-zero={byte === 0 || undefined} data-pending={byte === undefined || undefined}>{byte === undefined ? '··' : hexByte(byte)}</span>)
      text.push(<span key={column} className={css.hexChar} data-offset={offset} data-selected={selected} data-cursor={cursor}
        data-pending={byte === undefined || undefined}>{byte === undefined ? '·' : asciiByte(byte)}</span>)
    }
    rowsView.push(
      <div key={row} className={css.hexRow}>
        <span className={css.hexOffset}>{formatOffset(rowStart, size)}</span>
        <span className={css.hexBytes}>{hex}</span>
        <span className={css.hexAscii}>{text}</span>
      </div>,
    )
  }
  const offsetDigits = formatOffset(0, size).length
  const cursorOffset = selection?.focus

  return (
    <div className={css.viewer} data-viewer="hex">
      <div className={css.toolbar}>
        <span title={t('hexType')}>{signature === undefined ? t('loading') : signature === null ? t('hexUnknownType') : signature.label}</span>
        <span>· {fileSizeText(size)}</span>
        <span className={css.separator} />
        <input ref={gotoInput} aria-label={t('hexGoto')} placeholder={t('hexGotoPlaceholder')}
          onKeyDown={(event) => { if (event.key === 'Enter' && !isComposingKey(event.nativeEvent)) goto(event.currentTarget.value) }} />
        <select aria-label={t('hexCopy')} value="" disabled={selection === undefined}
          onChange={(event) => { if (event.target.value !== '') void copy(event.target.value as CopyFormat) }}>
          <option value="">{t('hexCopy')}</option>
          <option value="hex">{t('hexCopyHex')}</option>
          <option value="text">{t('hexCopyText')}</option>
          <option value="python">{t('hexCopyPython')}</option>
          <option value="c">{t('hexCopyC')}</option>
          <option value="base64">{t('hexCopyBase64')}</option>
        </select>
        <span className={css.grow} />
        {scan !== undefined && !scan.done
          ? <button type="button" className={css.toolButton} onClick={() => { scanning.current?.abort() }}>{t('hexScanStop')}</button>
          : <button type="button" className={css.toolButton} title={t('hexScanTitle')} disabled={size === 0} onClick={() => { void runScan() }}>{t('hexScan')}</button>}
        {onOpenText !== undefined && <select aria-label={t('hexOpenText')} title={t('hexOpenTextTitle')} value=""
          onChange={(event) => { if (event.target.value !== '') onOpenText(event.target.value) }}>
          <option value="">{t('hexOpenText')}</option>
          {TEXT_ENCODINGS.map(encoding => <option key={encoding} value={encoding}>{encoding.toUpperCase()}</option>)}
        </select>}
      </div>
      {trailing !== undefined && <div className={css.notice}>
        <span>{t('hexTrailing', { format: trailing.format, bytes: size - trailing.offset, offset: trailing.offset.toString(16).toUpperCase() })}</span>
        <button type="button" className={css.toolButton} onClick={() => { select(trailing.offset, size - 1) }}>{t('hexJump')}</button>
      </div>}
      {error !== '' && <div className={`${css.notice} ${css.error}`} role="alert">{t('loadFailed', { error })}</div>}
      <div className={css.hex}>
        <div ref={scroller} className={css.hexScroll} tabIndex={0} role="grid" aria-readonly="true"
          style={{ ['--hex-offset-width' as string]: `${offsetDigits + 2}ch` }}
          onScroll={(event) => { setScrollTop(event.currentTarget.scrollTop) }} onKeyDown={onKeyDown}
          onMouseDown={onMouseDown} onMouseMove={onMouseMove} onMouseUp={() => { dragging.current = false }}>
          {size === 0 ? <div className={css.message}>{fileSizeText(0)}</div> : (
            <div className={css.hexSpacer} style={{ height: total / scale }}>
              <div className={css.hexRows} style={{ transform: `translateY(${scale === 1 ? firstRow * ROW_HEIGHT : scrollTop}px)` }}>{rowsView}</div>
            </div>
          )}
        </div>
        <aside className={css.inspector} aria-label={t('hexInspector')}>
          <h3>{t('hexInspector')}</h3>
          {cursorOffset === undefined || start === undefined || end === undefined ? <p>{t('hexNoSelection')}</p> : <>
            <p>{t('hexSelection', { count: end - start + 1, start: start.toString(16).toUpperCase() })}</p>
            <table><tbody>
              <tr><td>{t('hexOffset')}</td><td>0x{start.toString(16).toUpperCase()} ({start})</td></tr>
              {inspected !== undefined && inspectBytes(inspected, littleEndian).map(row => <tr key={row.label}><td>{row.label}</td><td>{row.value}</td></tr>)}
            </tbody></table>
            <p>
              <button type="button" className={css.toolButton} aria-pressed={littleEndian} onClick={() => { setLittleEndian(true) }}>{t('hexLittle')}</button>
              <button type="button" className={css.toolButton} aria-pressed={!littleEndian} onClick={() => { setLittleEndian(false) }}>{t('hexBig')}</button>
            </p>
          </>}
          {scan !== undefined && <>
            <h3>{t('hexScan')}</h3>
            <p>{scan.done ? (scan.results.length === 0 ? t('hexScanNone') : t('hexScanFound', { count: scan.results.length })) : t('hexScanning', { percent: scan.percent })}</p>
            {scan.results.length >= MAX_SCAN_RESULTS && <p>{t('hexScanLimit', { count: MAX_SCAN_RESULTS })}</p>}
            <ul className={css.scanList}>{scan.results.map(match => (
              <li key={`${match.offset}:${match.label}`}>
                <button type="button" onClick={() => { select(match.offset); scroller.current?.focus() }}>
                  <code>0x{match.offset.toString(16).toUpperCase()}</code>{match.label}
                </button>
              </li>
            ))}</ul>
          </>}
        </aside>
      </div>
    </div>
  )
}
