/** Loopback HTTP server: launch-token auth, static files, routes and WebSocket upgrades. */
import { createHash, randomBytes, timingSafeEqual } from 'node:crypto'
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http'
import { readFile, stat } from 'node:fs/promises'
import type { Duplex } from 'node:stream'
import { extname, join, normalize, sep } from 'node:path'
import { WebSocketServer, type WebSocket } from 'ws'

/** Handles one authenticated request. */
export type RouteHandler = (request: IncomingMessage, response: ServerResponse, url: URL) => Promise<void> | void
/** Handles one authenticated WebSocket connection. */
export type SocketHandler = (socket: WebSocket, request: IncomingMessage, url: URL) => void

interface Route { path: string; exact: boolean; handler: RouteHandler }

const COOKIE = 'rainy_auth'

/** MIME types for files the Host serves. */
export const MIME: Readonly<Record<string, string>> = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.map': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml', '.png': 'image/png', '.jpg': 'image/jpeg', '.jpeg': 'image/jpeg', '.gif': 'image/gif',
  '.webp': 'image/webp', '.ico': 'image/x-icon', '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf',
  '.wasm': 'application/wasm', '.txt': 'text/plain; charset=utf-8', '.traineddata': 'application/octet-stream',
}

/** Options for {@link HostServer}. */
export interface HostServerOptions {
  /** Port to bind on 127.0.0.1; 0 picks a free port. */
  port: number
  /** Built renderer directory containing `index.html`. */
  rendererRoot: string
  /** Returns the inline script injected before the renderer bundle. */
  injectedGlobals: () => Record<string, unknown>
  log: (message: string) => void
}

/** The single loopback server of a Host. */
export class HostServer {
  private readonly server: Server
  private readonly routes: Route[] = []
  private readonly sockets = new Map<string, { wss: WebSocketServer; handler: SocketHandler }>()
  private readonly launchToken = randomBytes(32).toString('base64url')
  private readonly sessionSecret = randomBytes(32).toString('base64url')
  private boundPort = 0

  /** @param options Server configuration. */
  constructor(private readonly options: HostServerOptions) {
    this.server = createServer((request, response) => { void this.handle(request, response) })
    this.server.on('upgrade', (request, socket, head) => { this.upgrade(request, socket, head) })
  }

  /** @returns The bound port; valid after {@link listen}. */
  get port(): number { return this.boundPort }

  /** @returns The URL the carrier opens once; it exchanges the launch token for the session cookie. */
  launchUrl(): string {
    return `http://127.0.0.1:${this.boundPort}/?token=${this.launchToken}`
  }

  /**
   * Add a route. Longer prefixes win over shorter ones.
   * @param path Exact path or path prefix (ending in `/`).
   * @param handler Called for authenticated requests only.
   * @param exact Match only `path` itself.
   */
  route(path: string, handler: RouteHandler, exact = !path.endsWith('/')): void {
    this.routes.push({ path, exact, handler })
    this.routes.sort((left, right) => right.path.length - left.path.length)
  }

  /**
   * Accept WebSocket upgrades on one path.
   * @param path Exact request path.
   * @param handler Called for authenticated same-origin connections.
   */
  socket(path: string, handler: SocketHandler): void {
    this.sockets.set(path, { wss: new WebSocketServer({ noServer: true, maxPayload: 64 * 1024 * 1024 }), handler })
  }

  /** Bind to 127.0.0.1. */
  async listen(): Promise<void> {
    await new Promise<void>((resolve, reject) => {
      this.server.once('error', reject)
      this.server.listen(this.options.port, '127.0.0.1', () => { this.server.off('error', reject); resolve() })
    })
    const address = this.server.address()
    if (address === null || typeof address === 'string') throw new Error('Host server has no TCP address')
    this.boundPort = address.port
  }

  /** Stop accepting connections and close open sockets. */
  async close(): Promise<void> {
    for (const { wss } of this.sockets.values()) {
      for (const client of wss.clients) client.terminate()
      wss.close()
    }
    this.server.closeAllConnections()
    await new Promise<void>(resolve => { this.server.close(() => { resolve() }) })
  }

  private authorityOk(request: IncomingMessage): boolean {
    const host = request.headers.host
    return host === `127.0.0.1:${this.boundPort}` || host === `localhost:${this.boundPort}`
  }

  private authenticated(request: IncomingMessage): boolean {
    if (!this.authorityOk(request)) return false
    const header = request.headers.cookie ?? ''
    for (const part of header.split(';')) {
      const [name, ...value] = part.trim().split('=')
      if (name !== COOKIE) continue
      const given = Buffer.from(value.join('='))
      const expected = Buffer.from(this.sessionSecret)
      if (given.length === expected.length && timingSafeEqual(given, expected)) return true
    }
    return false
  }

  private async handle(request: IncomingMessage, response: ServerResponse): Promise<void> {
    response.setHeader('X-Content-Type-Options', 'nosniff')
    response.setHeader('Referrer-Policy', 'no-referrer')
    let url: URL
    try { url = new URL(request.url ?? '/', `http://127.0.0.1:${this.boundPort}`) } catch (_error) {
      response.writeHead(400); response.end(); return
    }
    try {
      if (url.pathname === '/' && url.searchParams.has('token') && this.authorityOk(request)) {
        const token = Buffer.from(url.searchParams.get('token') ?? '')
        const expected = Buffer.from(this.launchToken)
        if (token.length !== expected.length || !timingSafeEqual(token, expected)) { response.writeHead(403); response.end(); return }
        response.writeHead(302, {
          'Set-Cookie': `${COOKIE}=${this.sessionSecret}; Path=/; HttpOnly; SameSite=Strict`,
          Location: '/', 'Cache-Control': 'no-store',
        })
        response.end()
        return
      }
      if (!this.authenticated(request)) { response.writeHead(403); response.end(); return }
      const route = this.routes.find(item => item.exact ? url.pathname === item.path : url.pathname.startsWith(item.path))
      if (route !== undefined) { await route.handler(request, response, url); return }
      if (request.method !== 'GET' && request.method !== 'HEAD') { response.writeHead(405); response.end(); return }
      await this.serveRenderer(url, response)
    } catch (error) {
      this.options.log(`HTTP ${request.method} ${url.pathname} failed: ${error instanceof Error ? error.stack ?? error.message : String(error)}`)
      if (!response.headersSent) { response.writeHead(500); response.end() } else response.destroy()
    }
  }

  private async serveRenderer(url: URL, response: ServerResponse): Promise<void> {
    const root = this.options.rendererRoot
    const relative = url.pathname === '/' ? 'index.html' : decodeURIComponent(url.pathname.slice(1))
    const path = normalize(join(root, relative))
    if (path !== root && !path.startsWith(root + sep)) { response.writeHead(404); response.end(); return }
    let file = path
    let info = await stat(file).catch(() => undefined)
    if (info === undefined || !info.isFile()) {
      if (extname(relative) !== '') { response.writeHead(404); response.end(); return }
      file = join(root, 'index.html')
      info = await stat(file)
    }
    if (file.endsWith('index.html')) {
      const html = await readFile(file, 'utf8')
      const globals = Object.entries(this.options.injectedGlobals())
        .map(([name, value]) => `window.${name}=${JSON.stringify(value).replace(/</g, '\\u003c')};`).join('')
      const body = html.replace('<head>', `<head><script>${globals}</script>`)
      response.writeHead(200, { 'Content-Type': MIME['.html']!, 'Cache-Control': 'no-store' })
      response.end(body)
      return
    }
    response.writeHead(200, {
      'Content-Type': MIME[extname(file).toLowerCase()] ?? 'application/octet-stream',
      'Cache-Control': relative.startsWith('assets/') ? 'private, max-age=31536000, immutable' : 'no-cache',
      'Content-Length': info.size,
    })
    response.end(await readFile(file))
  }

  private upgrade(request: IncomingMessage, socket: Duplex, head: Buffer): void {
    let url: URL
    try { url = new URL(request.url ?? '/', `http://127.0.0.1:${this.boundPort}`) } catch (_error) { socket.destroy(); return }
    const entry = this.sockets.get(url.pathname)
    const origin = request.headers.origin
    const sameOrigin = origin === `http://127.0.0.1:${this.boundPort}` || origin === `http://localhost:${this.boundPort}`
    if (entry === undefined || !sameOrigin || !this.authenticated(request)) {
      socket.write('HTTP/1.1 403 Forbidden\r\n\r\n')
      socket.destroy()
      return
    }
    entry.wss.handleUpgrade(request, socket, head, (client) => { entry.handler(client, request, url) })
  }
}

/**
 * Read a request body with a size limit.
 * @param request Incoming request.
 * @param limit Maximum bytes.
 * @returns The body bytes.
 * @throws Error when the body exceeds `limit`.
 */
export async function readBody(request: IncomingMessage, limit: number): Promise<Buffer> {
  const chunks: Buffer[] = []
  let size = 0
  for await (const chunk of request) {
    const buffer = chunk as Buffer
    size += buffer.length
    if (size > limit) throw new Error('Request body too large')
    chunks.push(buffer)
  }
  return Buffer.concat(chunks)
}

/**
 * Send a JSON response.
 * @param response Server response.
 * @param status HTTP status.
 * @param value JSON-serializable body.
 */
export function sendJson(response: ServerResponse, status: number, value: unknown): void {
  response.writeHead(status, { 'Content-Type': MIME['.json']!, 'Cache-Control': 'no-store' })
  response.end(JSON.stringify(value))
}

/**
 * Strong ETag for file bytes.
 * @param bytes File content.
 * @returns A quoted ETag value.
 */
export function etagOf(bytes: Uint8Array): string {
  return `"${createHash('sha256').update(bytes).digest('base64url').slice(0, 27)}"`
}
