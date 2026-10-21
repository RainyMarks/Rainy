/** Exported Markdown documents: the page builds a self-contained HTML file, and this process prints it to PDF in a hidden
 *  window that runs no scripts and opens nothing else. */
import { BrowserWindow } from 'electron'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { searchablePdf } from './pdf-text.ts'

/**
 * @param name File name the page suggested, without its extension.
 * @returns It without characters Windows does not allow in file names; `document` when nothing is left.
 */
export function exportFileName(name: string): string {
  // Control characters and the ones Windows reserves.
  const cleaned = Array.from(name, character => character.charCodeAt(0) < 32 || '<>:"/\\|?*'.includes(character) ? '-' : character).join('')
    .replace(/[. ]+$/u, '').trim().slice(0, 120)
  return cleaned === '' ? 'document' : cleaned
}

/**
 * Print an HTML page to an A4 PDF with its headings as bookmarks and text that copies and searches as written.
 * @param html Complete document; scripts in it never run.
 * @returns The PDF.
 */
export async function printHtmlToPdf(html: string): Promise<Buffer> {
  const folder = await mkdtemp(join(tmpdir(), 'rainy-export-'))
  const window = new BrowserWindow({ show: false, width: 1000, height: 1400,
    webPreferences: { javascript: false, sandbox: true, contextIsolation: true, nodeIntegration: false, partition: 'rainy-export', spellcheck: false } })
  try {
    window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
    window.webContents.on('will-navigate', (event) => { event.preventDefault() })
    const page = join(folder, 'document.html')
    await writeFile(page, html, 'utf8')
    await window.loadFile(page)
    return searchablePdf(await window.webContents.printToPDF({ printBackground: true, preferCSSPageSize: true, pageSize: 'A4', generateDocumentOutline: true,
      generateTaggedPDF: true }))
  } finally {
    window.destroy()
    await rm(folder, { recursive: true, force: true }).catch(() => undefined)
  }
}
