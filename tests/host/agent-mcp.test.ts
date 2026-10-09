/** MCP servers from Settings connect over streamable HTTP with their request headers, expose allowed tools, and reject malformed headers. */
import { createServer, type IncomingHttpHeaders, type Server } from 'node:http'
import type { AddressInfo } from 'node:net'
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'
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
