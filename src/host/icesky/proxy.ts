/** Authenticated, non-persisting API relay for the bundled IceSky browser workbench. */
import type { IncomingMessage, ServerResponse } from 'node:http'
import { readRequestBytes } from './request-body.ts'
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-host-webserver'
import type {} from '@deepseek-ai/dsh-client-connection'

type Provider = 'openai' | 'anthropic'
type Operation = 'chat' | 'models'
const MAX_REQUEST = 128 * 1024
const MAX_RESPONSE = 4 * 1024 * 1024

function endpoint(provider: Provider, operation: Operation, baseHeader: string | string[] | undefined): URL {
  const fallback = provider === 'openai' ? 'https://api.openai.com/v1' : 'https://api.anthropic.com/v1'
  const base = new URL(typeof baseHeader === 'string' && baseHeader.trim() ? baseHeader : fallback)
  if (!['http:', 'https:'].includes(base.protocol) || base.username || base.password || base.search || base.hash) throw new Error('模型接口地址必须是 HTTP(S) Base URL。')
  return new URL(base.href.replace(/\/+$/, '') + (operation === 'models' ? '/models' : provider === 'openai' ? '/chat/completions' : '/messages'))
}

/**
 * Create one relay route; the browser supplies the key and base URL per request and the Host retains neither.
 * @param provider - upstream API family.
 * @param operation - relayed endpoint.
 * @param admit - connection admission, which runs before headers or the body are read.
 * @returns the handler for `/api/<provider>/<operation>`.
 */
export function createIceSkyProxyHandler(
  provider: Provider, operation: Operation, admit: (request: IncomingMessage) => 401 | 403 | undefined,
): (req: IncomingMessage, res: ServerResponse) => Promise<void> {
  return async (req, res) => {
    const rejection = admit(req)
    if (rejection !== undefined) { res.writeHead(rejection); res.end(); return }
    if (req.method !== (operation === 'chat' ? 'POST' : 'GET')) { res.writeHead(405); res.end(); return }
    const controller = new AbortController()
    res.on('close', () => { controller.abort() })
    try {
      const target = endpoint(provider, operation, req.headers[`x-${provider}-base-url`])
      const key = req.headers[`x-${provider}-api-key`]
      if (key !== undefined && typeof key !== 'string') throw new Error('模型密钥格式无效。')
      let body: string | undefined
      if (operation === 'chat') {
        body = (await readRequestBytes(req, MAX_REQUEST, () => new Error('模型请求过大。'))).toString('utf8')
        try { JSON.parse(body) } catch { throw new Error('模型请求 JSON 无效。') }
      }
      const headers: Record<string, string> = {}
      if (operation === 'chat') headers['content-type'] = 'application/json'
      if (provider === 'openai' && key) headers.authorization = `Bearer ${key}`
      if (provider === 'anthropic') {
        if (key) headers['x-api-key'] = key
        headers['anthropic-version'] = '2023-06-01'
      }
      const upstream = await fetch(target, { method: req.method, headers, body, signal: controller.signal, redirect: 'error' })
      res.writeHead(upstream.status, {
        'Content-Type': upstream.headers.get('content-type') ?? 'application/json; charset=utf-8',
        'Cache-Control': 'no-store',
        'X-Content-Type-Options': 'nosniff',
      })
      let total = 0
      if (upstream.body) {
        const reader = upstream.body.getReader()
        try {
          while (true) {
            const { done, value } = await reader.read()
            if (done) break
            total += value.byteLength
            if (total > MAX_RESPONSE) throw new Error('模型响应超过 4 MiB。')
            res.write(value)
          }
        } finally { reader.releaseLock() }
      }
      res.end()
    } catch (error) {
      if (res.headersSent) { res.destroy(); return }
      res.writeHead(502, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' })
      res.end(JSON.stringify({ error: { message: error instanceof Error ? error.message : '模型请求失败。' } }))
    }
  }
}

/** Register IceSky's four documented API routes without saving browser credentials. */
export function installIceSkyProxy(ctx: Context): void {
  const admit = (request: IncomingMessage): 401 | 403 | undefined => {
    const admission = ctx.connection.admit(request)
    return 'rejection' in admission ? admission.rejection : undefined
  }
  for (const provider of ['openai', 'anthropic'] as const) for (const operation of ['chat', 'models'] as const) {
    ctx.effect(() => ctx.webServer.register({ kind: 'exact', path: `/api/${provider}/${operation}`, handler: createIceSkyProxyHandler(provider, operation, admit) }))
  }
}
