/** Spreadsheet parsing (xlsx / xls / ods) off the main thread via a transient Web Worker, with microtask-chunked
 *  fallback in environments without Worker support (such as happy-dom). */
import { hydrateSheetGrids, MAX_SHEET_COLUMNS, MAX_SHEET_ROWS, parseWorkbookRaw, type SerializedSheetGrid, type SheetGrid } from './sheet-format.ts'

export { hydrateSheetGrids, MAX_SHEET_COLUMNS, MAX_SHEET_ROWS, type SerializedSheetGrid, type SheetGrid }

function nextMicroSlice(): Promise<void> {
  return new Promise(resolve => { setTimeout(resolve, 0) })
}

interface WorkerResponse {
  readonly id: number
  readonly sheets?: SerializedSheetGrid[] | undefined
  readonly error?: string | undefined
}

let workerRequestId = 1

/**
 * Try parsing in a one-shot Web Worker; returns `undefined` when Workers are unavailable or fail to spawn,
 * signaling the caller to fall back cleanly to chunked main-thread parsing.
 */
function parseInWorker(bytes: Uint8Array, locale: 'zh' | 'en', signal?: AbortSignal): Promise<SheetGrid[] | undefined> {
  if (typeof Worker === 'undefined') return Promise.resolve(undefined)
  let worker: Worker
  try {
    worker = new Worker(new URL('./sheet-worker.ts', import.meta.url))
  } catch {
    return Promise.resolve(undefined)
  }
  const id = workerRequestId++
  return new Promise<SheetGrid[] | undefined>((resolve, reject) => {
    let settled = false
    const finish = (callback: () => void): void => {
      if (settled) return
      settled = true
      signal?.removeEventListener('abort', onAbort)
      worker.terminate()
      callback()
    }
    const onAbort = (): void => {
      finish(() => { reject(new DOMException('Aborted', 'AbortError')) })
    }
    if (signal?.aborted) { onAbort(); return }
    signal?.addEventListener('abort', onAbort, { once: true })
    worker.onmessage = (event: MessageEvent<WorkerResponse>) => {
      const payload = event.data
      if (payload === null || typeof payload !== 'object' || payload.id !== id) return
      if (typeof payload.error === 'string') {
        finish(() => { reject(new Error(payload.error)) })
      } else if (Array.isArray(payload.sheets)) {
        finish(() => { resolve(hydrateSheetGrids(payload.sheets!)) })
      } else {
        finish(() => { resolve(undefined) })
      }
    }
    // Worker script load or CSP error: fall back transparently to chunked parsing.
    worker.onerror = () => { finish(() => { resolve(undefined) }) }
    worker.onmessageerror = () => { finish(() => { resolve(undefined) }) }
    try {
      const copy = bytes.slice()
      worker.postMessage({ id, bytes: copy, locale }, [copy.buffer])
    } catch {
      finish(() => { resolve(undefined) })
    }
  })
}

/**
 * Parse spreadsheet bytes in a transient Web Worker when available, gracefully degrading to chunked async parsing
 * on environments without Web Worker support.
 * @param bytes Spreadsheet bytes.
 * @param locale Formatting locale (`'zh'` or `'en'`).
 * @param signal Optional cancellation signal; aborts immediately and terminates any running worker.
 * @returns Parsed sheet grids.
 */
export async function readSheets(bytes: Uint8Array, locale: 'zh' | 'en', signal?: AbortSignal): Promise<SheetGrid[]> {
  if (signal?.aborted) throw new DOMException('Aborted', 'AbortError')
  const fromWorker = await parseInWorker(bytes, locale, signal)
  if (fromWorker !== undefined) return fromWorker
  if (signal?.aborted) throw new DOMException('Aborted', 'AbortError')
  const XLSX = await import('xlsx')
  return hydrateSheetGrids(await parseWorkbookRaw(XLSX, bytes, locale, nextMicroSlice, signal))
}
