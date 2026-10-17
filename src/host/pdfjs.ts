/** pdf.js for the PDF viewer, served from `resources/pdfjs` under `/rainy/pdfjs/`: the two browser files, the character
 *  maps for CJK fonts a PDF does not embed, and the standard fonts that stand in for the 14 base PDF fonts. */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import type { HostServer } from './server.ts'

/** Browser files; anything else under the route must match one of the data patterns below. */
const SCRIPTS = new Set(['pdf.min.js', 'pdf.worker.min.js'])
/** Binary character maps (`cmaps/UniGB-UCS2-H.bcmap`) and font programs (`standard_fonts/FoxitSans.pfb`); one plain name each. */
const DATA = /^(?:cmaps\/[A-Za-z0-9-]+\.bcmap|standard_fonts\/[A-Za-z0-9-]+\.(?:pfb|ttf))$/u

/**
 * Register `/rainy/pdfjs/`.
 * @param server Host server.
 * @param resources `<appRoot>/resources`.
 */
export function installPdfJs(server: HostServer, resources: string): void {
  server.route('/rainy/pdfjs/', async (request, response, url) => {
    const name = url.pathname.slice('/rainy/pdfjs/'.length)
    const script = SCRIPTS.has(name)
    if ((request.method !== 'GET' && request.method !== 'HEAD') || (!script && !DATA.test(name))) { response.writeHead(404); response.end(); return }
    let body: Buffer
    try { body = await readFile(join(resources, 'pdfjs', ...name.split('/'))) } catch {
      response.writeHead(404); response.end(); return
    }
    // The files change only with the application, which reloads the page.
    response.writeHead(200, { 'Content-Type': script ? 'text/javascript; charset=utf-8' : 'application/octet-stream', 'Content-Length': body.length,
      'Cache-Control': 'private, max-age=86400', 'X-Content-Type-Options': 'nosniff' })
    response.end(request.method === 'HEAD' ? undefined : body)
  })
}
