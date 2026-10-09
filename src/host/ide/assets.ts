/** Authenticated same-origin editor, worker, font and stylesheet resources. */
import { createReadStream } from 'node:fs'
import { realpath, stat } from 'node:fs/promises'
import type { IncomingMessage, ServerResponse } from 'node:http'
import { extname, resolve } from 'node:path'
import { pipeline } from 'node:stream/promises'
import { ideContains } from './ide-files-core.ts'

const mediaTypes: Readonly<Record<string, string>> = {
  '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8',
  '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.wasm': 'application/wasm',
  '.svg': 'image/svg+xml', '.png': 'image/png',
}

/** Serve only build-owned files under the fixed editor route.
 * @param options - resource root and existing authenticated request admission.
 * @returns the route handler; it opens no external connection and exposes no native API.
 */
export function createIdeAssetHandler(options: {
  readonly root: string
  readonly admit: (request: IncomingMessage) => 401 | 403 | undefined
}): (request: IncomingMessage, response: ServerResponse) => Promise<void> {
  return async (request, response) => {
    const rejection = options.admit(request)
    if (rejection !== undefined) { response.writeHead(rejection); response.end(); return }
    if (request.method !== 'GET' && request.method !== 'HEAD') { response.writeHead(405); response.end(); return }
    try {
      const url = new URL(request.url ?? '', 'http://localhost')
      const relative = decodeURIComponent(url.pathname.slice('/rainy/editor/'.length))
      if (!url.pathname.startsWith('/rainy/editor/') || !relative || relative.includes('\\') || relative.includes('\0')
        || relative.split('/').some(part => !part || part === '.' || part === '..')) {
        response.writeHead(404); response.end(); return
      }
      const root = await realpath(options.root)
      const target = await realpath(resolve(root, relative))
      const mime = mediaTypes[extname(target)]
      if (!ideContains(root, target) || !mime || !(await stat(target)).isFile()) {
        response.writeHead(404); response.end(); return
      }
      response.setHeader('Content-Type', mime)
      response.setHeader('X-Content-Type-Options', 'nosniff')
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
