/** Proxy choice for model requests: bypassed local hosts, system and manual proxies, and requests sent through a proxy. */
import { createServer, type IncomingHttpHeaders, type Server } from 'node:http'
import type { AddressInfo } from 'node:net'
import { afterEach, describe, expect, it } from 'vitest'
import { bypassesProxy, environmentProxy, Network, proxyUrl } from '../../src/host/network.ts'
import { proxyFromRules, systemProxyFor } from '../../src/main/system-proxy.ts'

describe('proxy settings', () => {
  it('keeps this machine, private networks, .local and single-label names direct', () => {
    for (const host of ['localhost', 'localhost.', '127.0.0.1', '10.1.2.3', '172.16.0.5', '192.168.1.10', '169.254.1.1', '[::1]', 'fd00::1',
      'fe90::1', '[::ffff:7f00:1]', '::ffff:192.168.0.2', 'strata.local', 'gpu-box', 'GPU-BOX.']) expect(bypassesProxy(host), host).toBe(true)
    for (const host of ['api.openai.com', '172.32.0.1', '8.8.8.8', 'example.localdomain', '[::ffff:808:808]', '2001:db8::1']) expect(bypassesProxy(host), host).toBe(false)
    expect(bypassesProxy(new URL('http://[::ffff:127.0.0.1]:8000/').hostname)).toBe(true)
  })

  it('accepts host:port or http URLs as proxy addresses', () => {
    expect(proxyUrl('127.0.0.1:7890')).toBe('http://127.0.0.1:7890')
    expect(proxyUrl('http://user:secret@proxy.lan:3128/')).toBe('http://user:secret@proxy.lan:3128')
    expect(proxyUrl('socks5://127.0.0.1:1080')).toBeUndefined()
    expect(proxyUrl('')).toBeUndefined()
  })

  it('reads the environment per scheme and honours NO_PROXY', () => {
    const env = { HTTPS_PROXY: 'http://secure:8080', http_proxy: 'http://plain:8080', NO_PROXY: '.corp.example, internal.test,*.lab' }
    expect(environmentProxy(env, new URL('https://api.openai.com/v1'))).toBe('http://secure:8080')
    expect(environmentProxy(env, new URL('http://api.example.com/'))).toBe('http://plain:8080')
    for (const url of ['https://llm.corp.example/', 'https://corp.example/', 'http://internal.test:8000/', 'https://gpu.lab/'])
      expect(environmentProxy(env, new URL(url)), url).toBeUndefined()
    expect(environmentProxy({ HTTPS_PROXY: 'http://secure:8080' }, new URL('http://plain.example/'))).toBe('http://secure:8080')
    expect(environmentProxy({ HTTPS_PROXY: 'http://secure:8080', NO_PROXY: '*' }, new URL('https://a.example/'))).toBeUndefined()
  })

  it('asks the Windows carrier per origin, reuses its answer, and falls back to the environment when it cannot answer', async () => {
    const asked: string[] = []
    let answer: string | null = 'http://127.0.0.1:7890'
    const network = new Network('win32', { HTTPS_PROXY: 'http://env-proxy:8080' }, (url) => {
      asked.push(url)
      return url.includes('broken') ? Promise.reject(new Error('no answer')) : Promise.resolve(url.includes('intranet') ? null : answer)
    })
    expect(await network.proxyFor({ mode: 'system' }, new URL('https://api.openai.com/v1/responses'))).toBe('http://127.0.0.1:7890')
    answer = 'http://changed:1'
    expect(await network.proxyFor({ mode: 'system' }, new URL('https://api.openai.com/v1/models'))).toBe('http://127.0.0.1:7890')
    expect(await network.proxyFor({ mode: 'system' }, new URL('https://intranet.corp.example/v1'))).toBeUndefined()
    expect(await network.proxyFor({ mode: 'system' }, new URL('https://broken.example/'))).toBe('http://env-proxy:8080')
    expect(await network.proxyFor({ mode: 'system' }, new URL('http://gpu-box:8000/v1'))).toBeUndefined()
    expect(asked).toEqual(['https://api.openai.com/', 'https://intranet.corp.example/', 'https://broken.example/'])
    expect(await network.proxyFor({ mode: 'off' }, new URL('https://api.openai.com/'))).toBeUndefined()
    expect(await network.proxyFor({ mode: 'manual', url: '10.0.0.2:3128' }, new URL('https://api.openai.com/'))).toBe('http://10.0.0.2:3128')
    const wsl = new Network('linux', {}, () => Promise.resolve('http://127.0.0.1:7890'))
    expect(await wsl.proxyFor({ mode: 'system' }, new URL('https://api.openai.com/'))).toBeUndefined()
  })

  it('masks a stored password and keeps it when the masked address comes back unchanged', async () => {
    const network = new Network('win32', {}, () => Promise.resolve(null))
    network.configure({ mode: 'manual', url: 'http://me:pw@corp:3128' })
    expect(await network.status('https://api.example.com/v1')).toEqual({
      settings: { mode: 'manual', url: 'http://me:***@corp:3128' }, effective: 'http://me:***@corp:3128', system: null,
    })
    expect(network.withStoredPassword({ mode: 'manual', url: 'http://me:***@corp:3128' })).toEqual({ mode: 'manual', url: 'http://me:pw@corp:3128' })
    expect(network.withStoredPassword({ mode: 'manual', url: 'http://other:***@corp:3128' })).toEqual({ mode: 'manual', url: 'http://other:***@corp:3128' })
    expect(network.withStoredPassword({ mode: 'manual', url: 'http://me:new@corp:3128' })).toEqual({ mode: 'manual', url: 'http://me:new@corp:3128' })
    expect((await network.status('http://127.0.0.1:8000/v1')).effective).toBeNull()
  })

  it('reads the first HTTP proxy from Chromium proxy rules', async () => {
    expect(proxyFromRules('PROXY 127.0.0.1:7890; DIRECT')).toBe('http://127.0.0.1:7890')
    expect(proxyFromRules('HTTPS proxy.corp:443')).toBe('https://proxy.corp:443')
    expect(proxyFromRules('DIRECT')).toBeNull()
    expect(proxyFromRules('SOCKS5 127.0.0.1:1080')).toBeNull()
    expect(await systemProxyFor({ resolveProxy: url => Promise.resolve(url.includes('corp') ? 'DIRECT' : 'PROXY 127.0.0.1:7890') }, 'https://llm.corp/')).toBeNull()
  })
})

describe('requests through a proxy', () => {
  const servers: Server[] = []
  afterEach(async () => {
    for (const server of servers.splice(0)) {
      server.closeAllConnections()
      await new Promise(done => { server.close(done) })
    }
  })

  async function listen(server: Server): Promise<number> {
    servers.push(server)
    await new Promise<void>(done => { server.listen(0, '127.0.0.1', done) })
    return (server.address() as AddressInfo).port
  }

  it('forwards plain-HTTP requests with the proxy credentials and leaves local hosts direct', async () => {
    const target = await listen(createServer((request, response) => { response.end(`direct ${request.headers.host}`) }))
    const seen: { url: string; headers: IncomingHttpHeaders }[] = []
    const tunnels: string[] = []
    const proxy = createServer((request, response) => {
      seen.push({ url: request.url ?? '', headers: request.headers })
      response.end(`proxied ${request.url}`)
    })
    proxy.on('connect', (request, socket) => { tunnels.push(request.url ?? ''); socket.destroy() })
    const proxyPort = await listen(proxy)
    const network = new Network('win32', {})
    network.configure({ mode: 'manual', url: `http://TOKEN@127.0.0.1:${proxyPort}` })
    const remote = await network.fetch('http://models.example.invalid:8080/v1/models')
    expect(await remote.text()).toBe('proxied http://models.example.invalid:8080/v1/models')
    expect(tunnels).toEqual([])
    expect(seen[0]?.headers['proxy-authorization']).toBe(`Basic ${Buffer.from('TOKEN:').toString('base64')}`)
    const local = await network.fetch(`http://127.0.0.1:${target}/`)
    expect(await local.text()).toBe(`direct 127.0.0.1:${target}`)
    expect(seen).toHaveLength(1)
    await expect(network.fetchWith({ mode: 'manual', url: '127.0.0.1:1' }, 'http://models.example.invalid/')).rejects.toThrow()
  })
})
