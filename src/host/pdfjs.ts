/** pdf.js for the PDF viewer, served from `resources/pdfjs` under `/rainy/pdfjs/`. */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import type { HostServer } from './server.ts'

/** The only files the viewer loads; anything else under the route is not found. */
const FILES = new Set(['pdf.min.js', 'pdf.worker.min.js'])

/**
 * Register `/rainy/pdfjs/`.
 * @param server Host server.
 * @param resources `<appRoot>/resources`.
 */
export function installPdfJs(server: HostServer, resources: string): void {
  server.route('/rainy/pdfjs/', async (request, response, url) => {
    const name = url.pathname.slice('/rainy/pdfjs/'.length)
    if ((request.method !== 'GET' && request.method !== 'HEAD') || !FILES.has(name)) { response.writeHead(404); response.end(); return }
    const body = await readFile(join(resources, 'pdfjs', name))
    // The files change only with the application, which reloads the page.
    response.writeHead(200, { 'Content-Type': 'text/javascript; charset=utf-8', 'Content-Length': body.length, 'Cache-Control': 'private, max-age=86400',
      'X-Content-Type-Options': 'nosniff' })
    response.end(request.method === 'HEAD' ? undefined : body)
  })
}
