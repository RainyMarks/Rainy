/** Non-persisting model API relay for the bundled IceSky workbench. */
import type { IncomingMessage, ServerResponse } from 'node:http'
import { readBody } from '../server.ts'
import { localized } from '../../shared/locale.ts'

/** Upstream API family. */
export type IceSkyProvider = 'openai' | 'anthropic'
/** Relayed endpoint. */
export type IceSkyOperation = 'chat' | 'models'
const MAX_REQUEST = 128 * 1024
const MAX_RESPONSE = 4 * 1024 * 1024

function endpoint(provider: IceSkyProvider, operation: IceSkyOperation, baseHeader: string | string[] | undefined): URL {
  const fallback = provider === 'openai' ? 'https://api.openai.com/v1' : 'https://api.anthropic.com/v1'
  const base = new URL(typeof baseHeader === 'string' && baseHeader.trim() ? baseHeader : fallback)
  if (!['http:', 'https:'].includes(base.protocol) || base.username || base.password || base.search || base.hash) throw new Error(localized({ zh: '模型接口地址必须是 HTTP(S) Base URL。', en: 'The model address must be an HTTP(S) base URL.' }))
  return new URL(base.href.replace(/\/+$/, '') + (operation === 'models' ? '/models' : provider === 'openai' ? '/chat/completions' : '/messages'))
}

/**
 * Create one relay route; the browser supplies the key and base URL per request and the Host keeps neither.
 * @param provider Upstream API family.
 * @param operation Relayed endpoint.
 * @returns The handler for `/api/<provider>/<operation>`; the server calls it only for authenticated requests.
 */
export function createIceSkyProxyHandler(provider: IceSkyProvider, operation: IceSkyOperation, request: typeof globalThis.fetch = globalThis.fetch): (req: IncomingMessage, res: ServerResponse) => Promise<void> {
  return async (req, res) => {
    if (req.method !== (operation === 'chat' ? 'POST' : 'GET')) { res.writeHead(405); res.end(); return }
    const controller = new AbortController()
    res.on('close', () => { controller.abort() })
    try {
      const target = endpoint(provider, operation, req.headers[`x-${provider}-base-url`])
      const key = req.headers[`x-${provider}-api-key`]
      if (key !== undefined && typeof key !== 'string') throw new Error(localized({ zh: '模型密钥格式无效。', en: 'The model key format is invalid.' }))
      let body: string | undefined
      if (operation === 'chat') {
        const declared = req.headers['content-length']
        if (declared !== undefined && (!/^\d+$/u.test(declared) || Number(declared) > MAX_REQUEST)) throw new Error(localized({ zh: '模型请求过大。', en: 'The model request is too large.' }))
        try { body = (await readBody(req, MAX_REQUEST)).toString('utf8') } catch (_tooLarge) { throw new Error(localized({ zh: '模型请求过大。', en: 'The model request is too large.' })) }
        try { JSON.parse(body) } catch (_invalidJson) { throw new Error(localized({ zh: '模型请求 JSON 无效。', en: 'The model request is not valid JSON.' })) }
      }
      const headers: Record<string, string> = {}
      if (operation === 'chat') headers['content-type'] = 'application/json'
      if (provider === 'openai' && key) headers.authorization = `Bearer ${key}`
      if (provider === 'anthropic') {
        if (key) headers['x-api-key'] = key
        headers['anthropic-version'] = '2023-06-01'
      }
      const upstream = await request(target, { method: req.method, headers, body, signal: controller.signal, redirect: 'error' })
      res.writeHead(upstream.status, {
        'Content-Type': upstream.headers.get('content-type') ?? 'application/json; charset=utf-8',
        'Cache-Control': 'no-store',
        'X-Content-Type-Options': 'nosniff',
      })
      let total = 0
      if (upstream.body) {
        const reader = upstream.body.getReader()
        try {
          for (;;) {
            const { done, value } = await reader.read()
            if (done) break
            total += value.byteLength
            if (total > MAX_RESPONSE) throw new Error(localized({ zh: '模型响应超过 4 MiB。', en: 'The model response is larger than 4 MiB.' }))
            res.write(value)
          }
        } finally { reader.releaseLock() }
      }
      res.end()
    } catch (error) {
      if (res.headersSent) { res.destroy(); return }
      res.writeHead(502, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store' })
      res.end(JSON.stringify({ error: { message: error instanceof Error ? error.message : localized({ zh: '模型请求失败。', en: 'The model request failed.' }) } }))
    }
  }
}
