/** `/rainy/ide/raw`: bytes of one workspace file for the IDE viewers, with single ranges for media seeking and hex paging. */
import type { IncomingMessage, ServerResponse } from 'node:http'
import { pipeline } from 'node:stream/promises'
import { z } from 'zod'
import { brandString } from '../../shared/brand.ts'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { ideMediaType } from '../../shared/ide-preview.ts'
import { ideFailure } from './files-core.ts'
import type { RainyIdeFiles } from './files.ts'

/** Served files stay inert: even a page navigated to one runs no script and gets no Host origin. */
const INERT_HEADERS: Readonly<Record<string, string>> = {
  'Content-Security-Policy': "sandbox; default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'",
  'X-Content-Type-Options': 'nosniff',
  'Cross-Origin-Resource-Policy': 'same-origin',
}

const querySchema = z.object({
  w: z.string().min(1).max(512).transform(value => brandString<WorkspaceId>(value)),
  r: z.string().min(1).max(128).transform(value => brandString<IdeRootId>(value)).optional(),
  p: z.string().min(1).max(32768),
  v: z.string().max(1024).optional(),
}).strict()

/**
 * Interpret a `Range` header. Only one `bytes` range is honored; anything else is served whole, as RFC 9110 allows.
 * @param header Request header value.
 * @param size File size in bytes.
 * @returns Inclusive byte bounds, `unsatisfiable`, or `undefined` for the whole file.
 */
export function parseByteRange(header: string | undefined, size: number): { start: number; end: number } | 'unsatisfiable' | undefined {
  const match = header === undefined ? null : /^bytes=(\d*)-(\d*)$/u.exec(header.trim())
  if (match === null) return undefined
  const [, first = '', last = ''] = match
  if (first === '' && last === '') return undefined
  if (first === '') {
    const suffix = Number(last)
    if (!Number.isSafeInteger(suffix) || suffix === 0) return 'unsatisfiable'
    return size === 0 ? 'unsatisfiable' : { start: Math.max(0, size - suffix), end: size - 1 }
  }
  const start = Number(first)
  const end = last === '' ? size - 1 : Math.min(Number(last), size - 1)
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start >= size || end < start) return 'unsatisfiable'
  return { start, end }
}

function etagMatches(header: string | undefined, etag: string): boolean {
  return header !== undefined && header.split(',').some(value => value.trim() === etag || value.trim() === '*')
}

function statusOf(code: string): number {
  if (code === 'version-conflict') return 409
  if (code === 'closed' || code === 'busy') return 503
  if (code === 'invalid-request' || code === 'invalid-path') return 400
  return 404
}

/**
 * Build the route handler.
 * @param files IDE file service, whose containment rules decide what may be served.
 * @param options.assertUsable Throws while the Host is switching execution targets.
 * @param options.signal Aborts open streams when the IDE closes.
 * @returns The handler for `HostServer.route`.
 */
export function createIdeRawRoute(files: RainyIdeFiles, options: { assertUsable: () => void; signal: AbortSignal }) {
  return async (request: IncomingMessage, response: ServerResponse, url: URL): Promise<void> => {
    for (const [name, value] of Object.entries(INERT_HEADERS)) response.setHeader(name, value)
    if (request.method !== 'GET' && request.method !== 'HEAD') { response.writeHead(405, { Allow: 'GET, HEAD' }); response.end(); return }
    const parsed = querySchema.safeParse(Object.fromEntries(url.searchParams))
    if (!parsed.success) { response.writeHead(400); response.end(); return }
    const query = parsed.data
    let opened: Awaited<ReturnType<RainyIdeFiles['openBytes']>>
    try {
      if (options.signal.aborted) throw Object.assign(new Error('The IDE is closing.'), { code: 'closed' })
      options.assertUsable()
      opened = await files.openBytes(query.w, query.r, query.p)
    } catch (error) {
      const code = error !== null && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : ideFailure(error).code
      response.writeHead(statusOf(code), { 'Cache-Control': 'no-store' }); response.end(); return
    }
    try {
      const etag = `"${opened.version}"`
      response.setHeader('Content-Type', ideMediaType(query.p))
      response.setHeader('Accept-Ranges', 'bytes')
      response.setHeader('ETag', etag)
      // A versioned URL names one observation of the file; an unversioned one is checked on every use.
      response.setHeader('Cache-Control', query.v === undefined ? 'private, no-cache' : 'private, max-age=3600')
      if (etagMatches(request.headers['if-none-match'], etag)) { response.writeHead(304); response.end(); return }
      const ifRange = request.headers['if-range']
      const range = ifRange === undefined || ifRange === etag ? parseByteRange(request.headers.range, opened.bytes) : undefined
      if (range === 'unsatisfiable') {
        response.writeHead(416, { 'Content-Range': `bytes */${opened.bytes}` }); response.end(); return
      }
      const start = range?.start ?? 0
      const end = range?.end ?? opened.bytes - 1
      response.setHeader('Content-Length', String(Math.max(0, end - start + 1)))
      if (range !== undefined) response.setHeader('Content-Range', `bytes ${start}-${end}/${opened.bytes}`)
      response.writeHead(range === undefined ? 200 : 206)
      if (request.method === 'HEAD' || opened.bytes === 0) { response.end(); return }
      const controller = new AbortController()
      const stop = (): void => { controller.abort() }
      options.signal.addEventListener('abort', stop, { once: true })
      response.once('close', () => { if (!response.writableFinished) stop() })
      try {
        await pipeline(opened.handle.createReadStream({ start, end, autoClose: false }), response, { signal: controller.signal })
      } catch (error) {
        if (!controller.signal.aborted) response.destroy(error instanceof Error ? error : undefined)
      } finally { options.signal.removeEventListener('abort', stop) }
    } finally {
      await opened.handle.close()
    }
  }
}
