/** Dedicated Web Worker for parsing Excel / OpenDocument spreadsheets off the UI thread.
 *  Static `xlsx` import avoids code-splitting inside Vite's default IIFE worker output. */
import * as XLSX from 'xlsx'
import { parseWorkbookRaw } from './sheet-format.ts'

interface WorkerRequest {
  readonly id: number
  readonly bytes: Uint8Array
  readonly locale: 'zh' | 'en'
}

self.addEventListener('message', (event: MessageEvent<WorkerRequest>) => {
  const { id, bytes, locale } = event.data
  void parseWorkbookRaw(XLSX, bytes, locale).then(
    (sheets) => { self.postMessage({ id, sheets }) },
    (error: unknown) => { self.postMessage({ id, error: error instanceof Error ? error.message : String(error) }) },
  )
})
