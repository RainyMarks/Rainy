/** Public loopback health checks shared by the Windows carrier and selected Rainy Host. */
import { z } from 'zod'
import type { StrataConnection } from './strata-protocol.ts'

const healthSchema = z.object({
  status: z.literal('ok'), service: z.literal('strata'), model: z.string().min(1),
  max_context: z.number().int().positive(), loaded: z.boolean(), api_key: z.boolean(),
})

/** Public metadata contains no credential, prompt, or generated answer. */
export interface StrataHealth extends StrataConnection {
  readonly loaded: boolean
  readonly authenticationRequired: boolean
}

/**
 * Resolve a Strata API root without permitting remote endpoints or embedded credentials.
 * @param baseURL - explicitly selected loopback API root ending in /v1.
 * @returns the normalized loopback URL.
 */
export function strataLoopbackURL(baseURL: string): URL {
  const url = new URL(baseURL)
  if (url.protocol !== 'http:' || !['127.0.0.1', 'localhost', '[::1]'].includes(url.hostname)
    || url.username || url.password || url.search || url.hash || !url.port || !/^\/v1\/?$/u.test(url.pathname)) {
    throw new Error('Strata 连接必须使用带端口的本机 HTTP 地址和 /v1 路径。')
  }
  url.pathname = '/v1'
  return url
}

/**
 * Inspect a local service without triggering model loading or inference.
 * @param baseURL - loopback API root.
 * @param options - optional fetch transport and caller cancellation for owned startup probes.
 * @returns validated public metadata, including an unloaded or authenticated external server.
 */
export async function readStrataHealth(baseURL: string, options: {
  fetcher?: typeof fetch
  signal?: AbortSignal
} = {}): Promise<StrataHealth> {
  const base = strataLoopbackURL(baseURL)
  const timeout = AbortSignal.timeout(3000)
  const response = await (options.fetcher ?? fetch)(new URL('/health', base), {
    signal: options.signal ? AbortSignal.any([options.signal, timeout]) : timeout,
    redirect: 'error',
  })
  if (!response.ok) throw new Error(`Strata 健康检查失败（HTTP ${response.status}）。`)
  const health = healthSchema.parse(await response.json())
  return { baseURL: base.href, model: health.model, contextWindow: health.max_context,
    loaded: health.loaded, authenticationRequired: health.api_key }
}

/**
 * Confirm this Host can reach a loaded, credential-free local Strata model before saving its provider.
 * @param baseURL - loopback API root supplied by the carrier.
 * @param options - optional fetch transport for focused protocol tests.
 * @returns the model and actual allocated context reported by Strata itself.
 */
export async function inspectStrataHealth(baseURL: string, options: { fetcher?: typeof fetch } = {}): Promise<StrataConnection> {
  const health = await readStrataHealth(baseURL, options)
  if (!health.loaded) throw new Error('Strata 模型尚未加载完成，请等待就绪后再连接。')
  if (health.authenticationRequired) throw new Error('此 Strata 服务需要 API 密钥，请在现有模型设置中配置凭据。')
  return { baseURL: health.baseURL, model: health.model, contextWindow: health.contextWindow }
}
