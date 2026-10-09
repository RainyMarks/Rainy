/** Local Strata discovery must not identify unrelated services or start model work. */
import { describe, expect, it, vi } from 'vitest'
import { inspectStrataHealth, readStrataHealth, strataLoopbackURL } from '../../src/main/strata-health.ts'

const health = { status: 'ok', service: 'strata', model: 'qwen3.8-flash-next', max_context: 32768, loaded: true, api_key: false }

describe('Strata loopback health', () => {
  it('reads only public health and uses the server context instead of a requested allocation', async () => {
    const fetcher = vi.fn<typeof fetch>(async () => Response.json(health))
    expect(await inspectStrataHealth('http://127.0.0.1:8081/v1/', { fetcher })).toEqual({
      baseURL: 'http://127.0.0.1:8081/v1', model: 'qwen3.8-flash-next', contextWindow: 32768,
    })
    expect(fetcher).toHaveBeenCalledExactlyOnceWith(new URL('http://127.0.0.1:8081/health'), expect.objectContaining({ redirect: 'error' }))
  })

  it.each(['https://127.0.0.1:8081/v1', 'http://example.com:8081/v1', 'http://127.0.0.1:8081/other',
    'http://user:secret@localhost:8081/v1', 'http://127.0.0.1:8081/v1?key=x'])('refuses a different endpoint %s', (baseURL) => {
    expect(() => strataLoopbackURL(baseURL)).toThrow()
  })

  it('reports an external authenticated server without claiming it is ready to configure', async () => {
    const fetcher = vi.fn<typeof fetch>(async () => Response.json({ ...health, api_key: true }))
    expect(await readStrataHealth('http://localhost:8081/v1', { fetcher })).toMatchObject({ authenticationRequired: true })
    await expect(inspectStrataHealth('http://localhost:8081/v1', { fetcher })).rejects.toThrow('API 密钥')
  })

  it('refuses unloaded and unrelated services without sending a load or completion request', async () => {
    const fetcher = vi.fn<typeof fetch>()
      .mockResolvedValueOnce(Response.json({ ...health, loaded: false }))
      .mockResolvedValueOnce(Response.json({ ...health, service: 'another-model-server' }))
    await expect(inspectStrataHealth('http://127.0.0.1:8081/v1', { fetcher })).rejects.toThrow('尚未加载')
    await expect(inspectStrataHealth('http://127.0.0.1:8081/v1', { fetcher })).rejects.toThrow()
    expect(fetcher.mock.calls.every(([url]) => url instanceof URL && url.pathname === '/health')).toBe(true)
  })
})
