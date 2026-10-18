/** The sandboxed frame of interactive answers and the libraries it may load, served from `resources/widget` under
 *  `/rainy/widget/`. Nothing here is private, and a sandboxed frame never sends the session cookie, so the route is open. */
import { readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { WIDGET_FRAME_PATH } from '../shared/widget-protocol.ts'
import type { HostServer } from './server.ts'

const PREFIX = '/rainy/widget/'
/** Library and font files; one plain name each, below `lib/` or `fonts/`. */
const FILES = /^(?:lib\/(?:[a-z0-9.-]+\.js|three-addons\/controls\/[A-Za-z]+\.js)|fonts\/[a-z0-9-]+\.woff2)$/u

/** Module paths for `import 'three'` and its orbit controls. */
const IMPORT_MAP = JSON.stringify({ imports: { three: `${PREFIX}lib/three.module.min.js`, 'three/addons/': `${PREFIX}lib/three-addons/` } })

/**
 * Policy of the frame: a sandbox with scripts but no same-origin access, popups, forms or top navigation; scripts, styles
 * and fonts only inline or from this route; images only inline; no network connections. The page's own `frame-src`
 * keeps the frame from navigating anywhere else.
 * @param port Host port.
 * @returns The header value.
 */
export function framePolicy(port: number): string {
  const own = `http://127.0.0.1:${port}${PREFIX} http://localhost:${port}${PREFIX}`
  return [
    'sandbox allow-scripts', "default-src 'none'", `script-src 'unsafe-inline' 'unsafe-eval' ${own}`, `style-src 'unsafe-inline' ${own}`,
    `font-src data: ${own}`, 'img-src data: blob:', 'media-src data: blob:', "connect-src 'none'", 'worker-src blob:', "frame-src 'none'",
    "form-action 'none'", "base-uri 'none'", `frame-ancestors http://127.0.0.1:${port} http://localhost:${port}`,
  ].join('; ')
}

/**
 * The frame document: base styles, the import map, the morphing library and the runtime, all inline so the frame is one
 * request.
 * @param resources `<appRoot>/resources`.
 * @returns HTML text.
 */
export async function frameDocument(resources: string): Promise<string> {
  const read = (name: string): Promise<string> => readFile(join(resources, 'widget', ...name.split('/')), 'utf8')
  const [css, morph, runtime] = await Promise.all([read('base.css'), read('lib/idiomorph.min.js'), read('runtime.js')])
  const script = (text: string): string => text.replace(/<\/script/giu, '<\\/script')
  return '<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width">'
    + `<style>${css}</style><script type="importmap">${IMPORT_MAP}</script>`
    + `<script>${script(morph)}</script><script>${script(runtime)}</script></head><body></body></html>`
}

/**
 * Register `/rainy/widget/`.
 * @param server Host server.
 * @param resources `<appRoot>/resources`.
 */
export function installWidget(server: HostServer, resources: string): void {
  let document: Promise<string> | undefined
  server.openRoute(PREFIX, async (request, response, url) => {
    if (request.method !== 'GET' && request.method !== 'HEAD') { response.writeHead(405); response.end(); return }
    if (url.pathname === WIDGET_FRAME_PATH) {
      document ??= frameDocument(resources)
      let body: Buffer
      try { body = Buffer.from(await document, 'utf8') } catch (error) {
        document = undefined
        throw error
      }
      response.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Content-Length': body.length, 'Cache-Control': 'no-cache',
        'Content-Security-Policy': framePolicy(server.port) })
      response.end(request.method === 'HEAD' ? undefined : body)
      return
    }
    const name = url.pathname.slice(PREFIX.length)
    if (!FILES.test(name)) { response.writeHead(404); response.end(); return }
    let body: Buffer
    try { body = await readFile(join(resources, 'widget', ...name.split('/'))) } catch {
      response.writeHead(404); response.end(); return
    }
    // Module scripts and fonts are fetched with CORS, and a sandboxed frame's origin is opaque.
    response.writeHead(200, {
      'Content-Type': name.endsWith('.js') ? 'text/javascript; charset=utf-8' : 'font/woff2', 'Content-Length': body.length,
      'Cache-Control': 'private, max-age=86400', 'Access-Control-Allow-Origin': '*', 'Cross-Origin-Resource-Policy': 'cross-origin',
    })
    response.end(request.method === 'HEAD' ? undefined : body)
  })
}
