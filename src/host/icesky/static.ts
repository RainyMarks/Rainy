/** Authenticated, content-versioned local browser resources with bounded streaming ownership. */
import { createHash } from 'node:crypto'
import { open, readFile } from 'node:fs/promises'
import type { IncomingMessage, ServerResponse } from 'node:http'
import { resolve } from 'node:path'
import { pipeline } from 'node:stream/promises'
import { fileURLToPath } from 'node:url'
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-host-webserver'
import type {} from '@deepseek-ai/dsh-client-connection'
import { z } from 'zod'

const assetSchema = z.object({
  sha256: z.string().regex(/^[a-f0-9]{64}$/), size: z.number().int().nonnegative(), type: z.string().min(1),
}).strict()
const manifestSchema = z.object({
  format: z.literal(1), version: z.string().regex(/^[a-f0-9]{64}$/), files: z.record(z.string(), assetSchema),
}).strict()
type AssetManifest = z.infer<typeof manifestSchema>

/** Static route dependencies; protected resources never bypass connection admission. */
export interface IceSkyStaticOptions {
  /** Admission precedes conditional cache responses and file access. */
  readonly admit: (request: IncomingMessage) => 401 | 403 | undefined
  /** Maximum age of content-versioned private browser resources. */
  readonly maxAgeSeconds: number
}

function safeName(name: string): boolean {
  return name.length > 0 && !name.startsWith('/') && !/[\\\x00]/.test(name)
    && !name.split('/').some(segment => segment === '.' || segment === '..' || segment === '.git' || segment === '')
}

function matches(request: IncomingMessage, etag: string): boolean {
  const value = request.headers['if-none-match']
  return typeof value === 'string' && value.split(',').some(item => item.trim() === '*' || item.trim().replace(/^W\//, '') === etag)
}

/** Owns manifest validation, two index variants and every in-flight static stream. */
export class IceSkyStaticAssets {
  private readonly streams = new Map<AbortController, Promise<void>>()
  private readonly indexes = new Map<boolean, { readonly bytes: Buffer; readonly etag: string }>()
  private closed = false

  private constructor(private readonly root: string, readonly version: string, private readonly files: AssetManifest['files']) {}

  /**
   * Validate built asset metadata and prepare the index without per-request file reads.
   * @param root - packaged resource directory.
   * @returns the static resource owner; missing or stale build metadata rejects boot.
   */
  static async open(root: string): Promise<IceSkyStaticAssets> {
    const manifest = manifestSchema.parse(JSON.parse(await readFile(resolve(root, 'assets-manifest.json'), 'utf8')))
    const digest = createHash('sha256').update(JSON.stringify(manifest.files)).digest('hex')
    if (digest !== manifest.version || !Object.hasOwn(manifest.files, 'index.html')
      || Object.keys(manifest.files).some(name => !safeName(name))) throw new Error('IceSky asset manifest is invalid.')
    const index = await readFile(resolve(root, 'index.html'))
    if (createHash('sha256').update(index).digest('hex') !== manifest.files['index.html'].sha256) throw new Error('IceSky index differs from the built resource manifest.')
    const owner = new IceSkyStaticAssets(root, manifest.version, manifest.files)
    for (const embedded of [false, true]) {
      const base = `<base href="/rainy/icesky/v/${manifest.version}/"><meta name="rainy-icesky-version" content="${manifest.version}">`
      const css = embedded ? '<link rel="stylesheet" href="css/rainy-embed.css">' : ''
      const bytes = Buffer.from(index.toString('utf8')
        .replace(/<head(?:\s[^>]*)?>/i, head => `${head}${base}`)
        .replace('</head>', `${css}</head>`))
      owner.indexes.set(embedded, { bytes, etag: `"${createHash('sha256').update(bytes).digest('hex')}"` })
    }
    return owner
  }

  /**
   * Serve a local resource; HEAD and matching ETag responses read no file body.
   * @param request - browser request, including an optional content-versioned URL.
   * @param response - Host response whose disconnect owns stream cancellation.
   * @param options - connection admission and immutable cache budget.
   */
  async handle(request: IncomingMessage, response: ServerResponse, options: IceSkyStaticOptions): Promise<void> {
    const rejection = options.admit(request)
    if (rejection !== undefined) { response.writeHead(rejection); response.end(); return }
    if (this.closed) { response.writeHead(503); response.end(); return }
    if (request.method !== 'GET' && request.method !== 'HEAD') { response.writeHead(405, { Allow: 'GET, HEAD' }); response.end(); return }
    try {
      const url = new URL(request.url ?? '/', 'http://localhost')
      let raw = decodeURIComponent(url.pathname.slice('/rainy/icesky'.length)).replace(/^\/+/, '') || 'index.html'
      let versioned = false
      if (raw.startsWith('v/')) {
        const [, version, ...parts] = raw.split('/')
        if (version !== this.version) { response.writeHead(410, { 'Cache-Control': 'no-store' }); response.end(); return }
        raw = parts.join('/')
        versioned = true
      }
      if (!safeName(raw) || !Object.hasOwn(this.files, raw)) { response.writeHead(404); response.end(); return }
      const asset = this.files[raw]
      const index = raw === 'index.html' ? this.indexes.get(url.searchParams.get('embed') === 'rainy') : undefined
      const etag = index?.etag ?? `"${asset.sha256}"`
      const type = `${asset.type}${asset.type.startsWith('text/') || asset.type === 'application/json' ? '; charset=utf-8' : ''}`
      response.setHeader('Content-Type', type)
      response.setHeader('X-Content-Type-Options', 'nosniff')
      response.setHeader('ETag', etag)
      response.setHeader('Cache-Control', versioned && index === undefined ? `private, max-age=${options.maxAgeSeconds}, immutable` : 'private, no-cache')
      if (matches(request, etag)) { response.writeHead(304); response.end(); return }
      response.setHeader('Content-Length', index?.bytes.length ?? asset.size)
      if (request.method === 'HEAD') { response.writeHead(200); response.end(); return }
      if (index) { response.writeHead(200); response.end(index.bytes); return }
      // Registered before the file opens, so close() also stops a request that is still opening its file.
      const controller = new AbortController()
      const job = this.stream(resolve(this.root, raw), response, controller)
      this.streams.set(controller, job)
      try { await job }
      finally { this.streams.delete(controller) }
    } catch (error) {
      if (response.destroyed) return
      if (response.headersSent) { response.destroy(error instanceof Error ? error : undefined); return }
      response.removeHeader('Content-Length')
      response.removeHeader('ETag')
      response.setHeader('Cache-Control', 'no-store')
      response.writeHead(404); response.end()
    }
  }

  private async stream(path: string, response: ServerResponse, controller: AbortController): Promise<void> {
    const file = await open(path, 'r')
    const disconnected = (): void => { if (!response.writableFinished) controller.abort() }
    response.once('close', disconnected)
    try { await pipeline(file.createReadStream(), response, { signal: controller.signal }) }
    finally { response.removeListener('close', disconnected); await file.close() }
  }

  /** @returns resolution after owned streams stop; new requests reject immediately. */
  async close(): Promise<void> {
    this.closed = true
    for (const controller of this.streams.keys()) controller.abort()
    await Promise.allSettled(this.streams.values())
  }
}

/**
 * Register the workbench's versioned resource route and its teardown ownership.
 * @param ctx - authenticated HTTP Host.
 * @param maxAgeSeconds - immutable private cache lifetime for versioned resources.
 */
export async function installIceSkyStatic(ctx: Context, maxAgeSeconds: number): Promise<void> {
  const owner = await IceSkyStaticAssets.open(fileURLToPath(new URL('../resources/icesky/', import.meta.url)))
  ctx.effect(() => {
    const unregister = ctx.webServer.register({ kind: 'prefix', path: '/rainy/icesky', handler: (request, response) => owner.handle(request, response, {
      admit: (request) => { const result = ctx.connection.admit(request); return 'rejection' in result ? result.rejection : undefined }, maxAgeSeconds,
    }) })
    return async () => { unregister(); await owner.close() }
  })
}
