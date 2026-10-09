/** Serve one installed offline webpage from a private, bounded loopback resource root. */
import { createServer } from 'node:http'
import { dirname, extname, isAbsolute, relative, resolve, sep } from 'node:path'
import { randomBytes } from 'node:crypto'
import { finished, pipeline } from 'node:stream/promises'
import type { NativeInvocation } from './native-tools.ts'
import { toolPackFileSystem } from './toolpack-fs.ts'

const { createReadStream } = toolPackFileSystem
const { realpath, stat } = toolPackFileSystem.promises

const contentTypes: Readonly<Record<string, string>> = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.svg': 'image/svg+xml', '.png': 'image/png',
  '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif', '.webp': 'image/webp', '.ico': 'image/x-icon',
  '.wasm': 'application/wasm', '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.otf': 'font/otf',
  '.txt': 'text/plain; charset=utf-8', '.xml': 'application/xml', '.map': 'application/json', '.mp3': 'audio/mpeg', '.wav': 'audio/wav',
}

/** Resources owned by one offline tool window. */
export interface NativeToolWebPage {
  readonly url: string
  readonly origin: string
  /** Stop accepting requests and wait for every owned connection to close. */
  close(): Promise<void>
}

/**
 * Start a private static server for one checked offline tool.
 * @param invocation - installed HTML entry and contained tool roots.
 * @returns its private entry URL and complete asynchronous cleanup.
 */
export async function serveNativeTool(invocation: NativeInvocation): Promise<NativeToolWebPage> {
  if (invocation.kind !== 'web') throw new Error('只支持离线网页工具')
  const root = await realpath(dirname(invocation.target))
  const entry = relative(root, await realpath(invocation.target))
  const prefix = `/${randomBytes(24).toString('hex')}/`
  let closed = false
  const requests = new Set<Promise<void>>()
  const server = createServer((request, response) => {
    const interrupted = (): boolean => closed || response.destroyed
    const operation = (async () => {
      if (interrupted()) { response.destroy(); return }
      if (request.method !== 'GET' && request.method !== 'HEAD') { response.writeHead(405).end(); return }
      const path = new URL(request.url ?? '/', 'http://127.0.0.1').pathname
      if (!path.startsWith(prefix)) { response.writeHead(404).end(); return }
      const relativePath = decodeURIComponent(path.slice(prefix.length))
      if (relativePath.includes('\0') || relativePath.includes('\\') || relativePath.includes(':') || relativePath.split('/').some(value => value === '..')) {
        response.writeHead(404).end(); return
      }
      const target = await realpath(resolve(root, relativePath))
      if (interrupted()) return
      const suffix = relative(root, target)
      if (suffix.startsWith(`..${sep}`) || suffix === '..' || isAbsolute(suffix)) { response.writeHead(404).end(); return }
      const info = await stat(target)
      if (interrupted()) return
      if (!info.isFile()) { response.writeHead(404).end(); return }
      response.writeHead(200, { 'Content-Type': contentTypes[extname(target).toLowerCase()] ?? 'application/octet-stream',
        'Content-Length': info.size, 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer',
        'Content-Security-Policy': "default-src 'self' data: blob:; script-src 'self' 'unsafe-inline' 'unsafe-eval' blob:; style-src 'self' 'unsafe-inline'; connect-src 'self' data: blob:; object-src 'none'; frame-ancestors 'none'" })
      if (request.method === 'HEAD') { response.end(); return }
      const stream = createReadStream(target)
      // pipeline reports request errors; finished also observes descriptor closure if piping fails immediately.
      const streamClosed = finished(stream, { cleanup: true }).catch((error: unknown) => error)
      try { await pipeline(stream, response) }
      finally { stream.destroy(); await streamClosed }
    })().catch((error: unknown) => {
      if (interrupted()) return
      if (!response.headersSent) response.writeHead(error instanceof URIError ? 400 : 404)
      response.end()
    }).finally(() => { requests.delete(operation) })
    requests.add(operation)
  })
  await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', () => { server.removeListener('error', reject); resolve() }) })
  const address = server.address()
  if (address === null || typeof address === 'string') { server.close(); throw new Error('无法打开离线工具页面') }
  const origin = `http://127.0.0.1:${address.port}`
  let closing: Promise<void> | undefined
  return { origin, url: `${origin}${prefix}${entry.split(sep).map(encodeURIComponent).join('/')}`,
    close: () => {
      if (closing) return closing
      closed = true
      closing = Promise.resolve().then(async () => {
        await new Promise<void>((resolve, reject) => {
          server.close((error) => { if (error) reject(error); else resolve() })
          server.closeAllConnections()
        })
        await Promise.all([...requests])
      })
      return closing
    },
  }
}
