/** MCP servers from Settings connect over streamable HTTP (or the older HTTP+SSE) with their request headers, expose allowed tools, and reject malformed headers. */
import { createServer, type IncomingHttpHeaders, type Server } from 'node:http'
import type { AddressInfo } from 'node:net'
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'
import { SSEServerTransport } from '@modelcontextprotocol/sdk/server/sse.js'
import { StreamableHTTPServerTransport } from '@modelcontextprotocol/sdk/server/streamableHttp.js'
import { afterEach, expect, it } from 'vitest'
import { z } from 'zod'
import type { McpServerConfig } from '../../src/shared/rpc.ts'
import { McpManager } from '../../src/host/agent/mcp.ts'
import { Settings } from '../../src/host/settings.ts'
import { tempHost } from './agent-fixtures.ts'

const cleanups: (() => Promise<unknown>)[] = []
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup() })

/** A stateless MCP server with one `echo` tool that records the headers of every request. */
async function echoServer(): Promise<{ url: string; requests: IncomingHttpHeaders[] }> {
  const requests: IncomingHttpHeaders[] = []
  const http: Server = createServer((request, response) => {
    requests.push(request.headers)
    const chunks: Buffer[] = []
    request.on('data', (chunk: Buffer) => { chunks.push(chunk) })
    request.on('end', () => {
      const mcp = new McpServer({ name: 'echo', version: '1' })
      mcp.registerTool('echo', { description: 'Echo text', inputSchema: { text: z.string() } }, ({ text }) => ({ content: [{ type: 'text', text }] }))
      mcp.registerTool('hidden', { description: 'Not allowed' }, () => ({ content: [{ type: 'text', text: 'no' }] }))
      const transport = new StreamableHTTPServerTransport({ sessionIdGenerator: undefined })
      response.on('close', () => { void transport.close(); void mcp.close() })
      const body = chunks.length === 0 ? undefined : JSON.parse(Buffer.concat(chunks).toString('utf8')) as unknown
      void mcp.connect(transport).then(() => transport.handleRequest(request, response, body))
    })
  })
  await new Promise<void>((resolve) => { http.listen(0, '127.0.0.1', resolve) })
  cleanups.push(() => new Promise((resolve) => { http.closeAllConnections(); http.close(resolve) }))
  return { url: `http://127.0.0.1:${(http.address() as AddressInfo).port}/mcp`, requests }
}

async function ready(manager: McpManager, name: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt++) {
    const state = manager.status().find(server => server.name === name)
    if (state?.state === 'ready') return
    if (state?.state === 'error') throw new Error(state.error)
    await new Promise(resolve => setTimeout(resolve, 25))
  }
  throw new Error(`${name} did not connect`)
}

it('sends the configured headers on every request and exposes only allowed tools', async () => {
  const { url, requests } = await echoServer()
  const manager = new McpManager(() => undefined)
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'web', enabled: true, transport: 'streamable-http', url, headers: { Authorization: 'Bearer token-1', 'X-Team': 'rainy' }, tools: ['echo'] }])
  await ready(manager, 'web')

  expect(manager.status()[0]?.tools).toEqual([
    { name: 'echo', description: 'Echo text', enabled: true },
    { name: 'hidden', description: 'Not allowed', enabled: false },
  ])
  const tools = manager.tools()
  expect(tools.map(tool => tool.name)).toEqual(['mcp__web__echo'])
  const result = await tools[0]!.execute('call-1', { text: 'flag{headers}' })
  expect(result.content).toEqual([{ type: 'text', text: 'flag{headers}' }])
  expect(requests.length).toBeGreaterThanOrEqual(3)
  for (const headers of requests) expect(headers).toMatchObject({ authorization: 'Bearer token-1', 'x-team': 'rainy' })
})

it('saves valid headers and rejects malformed or case-duplicate names', async () => {
  const host = await tempHost()
  cleanups.push(host.cleanup)
  const settings = new Settings(host.env.home, {})
  await settings.load()
  const server = (headers: Record<string, string>): McpServerConfig =>
    ({ name: 'web', enabled: true, transport: 'streamable-http', url: 'https://mcp.example.test/mcp', headers, tools: [] })
  await expect(settings.update((data) => { data.mcpServers = [server({ 'bad header': 'x' })] })).rejects.toThrow('valid, distinct')
  await expect(settings.update((data) => { data.mcpServers = [server({ 'X-Key': 'line\nbreak' })] })).rejects.toThrow('valid, distinct')
  await expect(settings.update((data) => { data.mcpServers = [server({ 'X-Key': 'one', 'x-key': 'two' })] })).rejects.toThrow('valid, distinct')
  await settings.update((data) => { data.mcpServers = [server({ Authorization: 'Bearer token-1' })] })
  const reloaded = new Settings(host.env.home, {})
  await reloaded.load()
  expect(reloaded.get().mcpServers[0]?.headers).toEqual({ Authorization: 'Bearer token-1' })
})

it('answers whether a tool is still allowed, so a run refuses one turned off after it started', async () => {
  const { url } = await echoServer()
  const manager = new McpManager(() => undefined)
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'web', enabled: true, transport: 'streamable-http', url, tools: ['echo'] }])
  await ready(manager, 'web')
  expect(manager.allows('mcp__web__echo')).toBe(true)
  expect(manager.allows('mcp__web__hidden')).toBe(false)
  manager.sync([{ name: 'web', enabled: true, transport: 'streamable-http', url, tools: ['hidden'] }])
  expect(manager.allows('mcp__web__echo')).toBe(false)
})

/** An MCP server that speaks only the older HTTP+SSE transport: GET opens the stream, POST to its endpoint sends messages. */
async function sseServer(): Promise<{ url: string; requests: IncomingHttpHeaders[]; sessions: import('node:http').ServerResponse[] }> {
  const requests: IncomingHttpHeaders[] = []
  const sessions: import('node:http').ServerResponse[] = []
  const transports = new Map<string, SSEServerTransport>()
  const http: Server = createServer((request, response) => {
    requests.push(request.headers)
    const url = new URL(request.url ?? '/', 'http://127.0.0.1')
    if (request.method === 'GET' && url.pathname === '/sse') {
      const mcp = new McpServer({ name: 'old', version: '1' })
      mcp.registerTool('echo', { description: 'Echo text', inputSchema: { text: z.string() } }, ({ text }) => ({ content: [{ type: 'text', text }] }))
      const transport = new SSEServerTransport('/messages', response)
      transports.set(transport.sessionId, transport)
      sessions.push(response)
      response.on('close', () => { transports.delete(transport.sessionId); void mcp.close() })
      void mcp.connect(transport)
      return
    }
    const transport = transports.get(url.searchParams.get('sessionId') ?? '')
    if (request.method === 'POST' && url.pathname === '/messages' && transport !== undefined) { void transport.handlePostMessage(request, response); return }
    response.writeHead(405).end('Method Not Allowed')
  })
  await new Promise<void>((resolve) => { http.listen(0, '127.0.0.1', resolve) })
  cleanups.push(() => new Promise((resolve) => { http.closeAllConnections(); http.close(resolve) }))
  return { url: `http://127.0.0.1:${(http.address() as AddressInfo).port}/sse`, requests, sessions }
}

it('falls back to HTTP+SSE for a server that refuses Streamable HTTP, keeping the headers', async () => {
  const { url, requests } = await sseServer()
  const manager = new McpManager(() => undefined)
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'old', enabled: true, transport: 'streamable-http', url, headers: { Authorization: 'Bearer token-2' }, tools: [] }])
  await ready(manager, 'old')
  const tools = manager.tools()
  expect(tools.map(tool => tool.name)).toEqual(['mcp__old__echo'])
  const result = await tools[0]!.execute('call-1', { text: 'over sse' })
  expect(result.content).toEqual([{ type: 'text', text: 'over sse' }])
  for (const headers of requests) expect(headers).toMatchObject({ authorization: 'Bearer token-2' })
})

it('reports the Streamable HTTP error when the address speaks neither transport', async () => {
  const http: Server = createServer((_request, response) => { response.writeHead(404).end('no such page') })
  await new Promise<void>((resolve) => { http.listen(0, '127.0.0.1', resolve) })
  cleanups.push(() => new Promise((resolve) => { http.closeAllConnections(); http.close(resolve) }))
  const manager = new McpManager(() => undefined)
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'gone', enabled: true, transport: 'streamable-http', url: `http://127.0.0.1:${(http.address() as AddressInfo).port}/mcp`, tools: [] }])
  await expect(ready(manager, 'gone')).rejects.toThrow(/Error POSTing to endpoint/u)
})

/** A server whose POST and GET answer as given; `streams` counts GET event streams still open. */
async function oddServer(post: number, get: (response: import('node:http').ServerResponse) => void): Promise<{ url: string; streams: () => number }> {
  let open = 0
  const http: Server = createServer((request, response) => {
    if (request.method === 'GET') { open++; response.on('close', () => { open-- }); get(response); return }
    response.writeHead(post).end('no')
  })
  await new Promise<void>((resolve) => { http.listen(0, '127.0.0.1', resolve) })
  cleanups.push(() => new Promise((resolve) => { http.closeAllConnections(); http.close(resolve) }))
  return { url: `http://127.0.0.1:${(http.address() as AddressInfo).port}/sse`, streams: () => open }
}
/** An event stream that stays open without ever naming its session endpoint. */
const silentStream = (response: import('node:http').ServerResponse): void => { response.writeHead(200, { 'content-type': 'text/event-stream' }); response.write(': keep-alive\n\n') }

it('gives up on an HTTP+SSE stream that never names its session within the connect timeout, and closes it', async () => {
  const { url, streams } = await oddServer(429, silentStream)
  const manager = new McpManager(() => undefined, globalThis.fetch, { connectMs: 1500, sseSettleMs: 10_000 })
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'silent', enabled: true, transport: 'streamable-http', url, tools: [] }])
  await expect(ready(manager, 'silent')).rejects.toThrow(/HTTP\+SSE/u)
  for (let attempt = 0; attempt < 40 && streams() > 0; attempt++) await new Promise(resolve => setTimeout(resolve, 25))
  expect(streams()).toBe(0)
})

it('ends a connection attempt still waiting on HTTP+SSE when the server is turned off', async () => {
  const { url, streams } = await oddServer(429, silentStream)
  const manager = new McpManager(() => undefined, globalThis.fetch, { connectMs: 30_000, sseSettleMs: 10_000 })
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'silent', enabled: true, transport: 'streamable-http', url, tools: [] }])
  for (let attempt = 0; attempt < 80 && streams() === 0; attempt++) await new Promise(resolve => setTimeout(resolve, 25))
  expect(streams()).toBe(1)
  manager.sync([{ name: 'silent', enabled: false, transport: 'streamable-http', url, tools: [] }])
  for (let attempt = 0; attempt < 40 && streams() > 0; attempt++) await new Promise(resolve => setTimeout(resolve, 25))
  expect(streams()).toBe(0)
})

it('reports why HTTP+SSE refused, such as a rejected token, rather than the POST that was never meant to work', async () => {
  const { url } = await oddServer(404, (response) => { response.writeHead(401).end('bad token') })
  const manager = new McpManager(() => undefined, globalThis.fetch, { connectMs: 5000, sseSettleMs: 10_000 })
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'locked', enabled: true, transport: 'streamable-http', url, tools: [] }])
  await expect(ready(manager, 'locked')).rejects.toThrow(/401/u)
})

it('connects again with a fresh session when an HTTP+SSE stream drops', async () => {
  const { url, sessions } = await sseServer()
  const manager = new McpManager(() => undefined, globalThis.fetch, { connectMs: 5000, sseSettleMs: 0 })
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'old', enabled: true, transport: 'streamable-http', url, tools: [] }])
  await ready(manager, 'old')
  const first = sessions.at(-1)!
  first.destroy()
  for (let attempt = 0; attempt < 200 && sessions.length < 2; attempt++) await new Promise(resolve => setTimeout(resolve, 25))
  await ready(manager, 'old')
  const result = await manager.tools()[0]!.execute('call-2', { text: 'after reconnect' })
  expect(result.content).toEqual([{ type: 'text', text: 'after reconnect' }])
  expect(sessions.length).toBe(2)
})

it('says which command did not start instead of a bare connection error', async () => {
  const manager = new McpManager(() => undefined)
  cleanups.push(() => manager.close())
  manager.sync([{ name: 'missing', enabled: true, transport: 'stdio', command: 'rainy-no-such-command-qa', args: [], tools: [] }])
  await expect(ready(manager, 'missing')).rejects.toThrow(/找不到启动命令 rainy-no-such-command-qa|启动命令运行后立即退出了/u)
})
