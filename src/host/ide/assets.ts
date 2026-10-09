/** Same-origin editor bundle, workers, fonts and stylesheets under `/rainy/editor/`. */
import { createReadStream } from 'node:fs'
import { realpath, stat } from 'node:fs/promises'
import type { IncomingMessage, ServerResponse } from 'node:http'
import { extname, resolve } from 'node:path'
import { pipeline } from 'node:stream/promises'
import { ideContains } from './files-core.ts'

const mediaTypes: Readonly<Record<string, string>> = {
  '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8',
  '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.wasm': 'application/wasm',
  '.svg': 'image/svg+xml', '.png': 'image/png',
}

/**
 * Serve only build-owned files from the editor bundle directory.
 * @param root Directory of the built editor (`<resources>/editor`).
 * @returns The route handler for authenticated requests.
 */
export function createIdeAssetHandler(root: string): (request: IncomingMessage, response: ServerResponse, url: URL) => Promise<void> {
  return async (request, response, url) => {
    if (request.method !== 'GET' && request.method !== 'HEAD') { response.writeHead(405); response.end(); return }
    try {
      const relative = decodeURIComponent(url.pathname.slice('/rainy/editor/'.length))
      if (!url.pathname.startsWith('/rainy/editor/') || !relative || relative.includes('\\') || relative.includes('\0')
        || relative.split('/').some(part => !part || part === '.' || part === '..')) {
        response.writeHead(404); response.end(); return
      }
      const base = await realpath(root)
      const target = await realpath(resolve(base, relative))
      const mime = mediaTypes[extname(target)]
      if (!ideContains(base, target) || !mime || !(await stat(target)).isFile()) {
        response.writeHead(404); response.end(); return
      }
      response.setHeader('Content-Type', mime)
      response.setHeader('Cross-Origin-Resource-Policy', 'same-origin')
      response.setHeader('Cache-Control', 'no-cache')
      if (request.method === 'HEAD') { response.end(); return }
      await pipeline(createReadStream(target), response)
    } catch (error) {
      if (response.destroyed) return
      const missing = error instanceof Error && 'code' in error && error.code === 'ENOENT'
      if (!response.headersSent) response.writeHead(missing || error instanceof URIError ? 404 : 500)
      response.end()
    }
  }
}
