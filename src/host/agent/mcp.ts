/** MCP servers configured in Settings: connections, their tools as agent tools, and their instructions. */
import { createHash } from 'node:crypto'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { SSEClientTransport, SseError } from '@modelcontextprotocol/sdk/client/sse.js'
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js'
import { StreamableHTTPClientTransport, StreamableHTTPError } from '@modelcontextprotocol/sdk/client/streamableHttp.js'
import type { AgentTool } from '@earendil-works/pi-agent-core'
import type { ImageContent, TextContent, TSchema } from '@earendil-works/pi-ai'
import { localized } from '../../shared/locale.ts'
import type { McpServerConfig, McpServerStatus } from '../../shared/rpc.ts'
import { consoleText, scrubbedEnv } from '../process.ts'

/** Per-call timeout for MCP tools. */
const CALL_TIMEOUT_MS = 60_000
const CALL_TOTAL_TIMEOUT_MS = 30 * 60_000
/** Wait for a server to start and list its tools. */
const CONNECT_TIMEOUT_MS = 60_000
/** A local server started through uvx or npx can spend its first start downloading packages and an interpreter. */
const STDIO_CONNECT_TIMEOUT_MS = 180_000

/**
 * Model-facing name of an MCP tool.
 * @param server Server name.
 * @param tool Tool name.
 * @returns `mcp__<server>__<tool>` restricted to `[A-Za-z0-9_-]`, at most 64 characters.
 */
export function mcpToolName(server: string, tool: string): string {
  const name = `mcp__${server}__${tool}`.replace(/[^A-Za-z0-9_-]/g, '_')
  if (name.length <= 64) return name
  return `${name.slice(0, 55)}_${createHash('sha256').update(name).digest('hex').slice(0, 8)}`
}

interface Connection {
  config: McpServerConfig
  client?: Client | undefined
  state: McpServerStatus['state']
  error?: string | undefined
  /** `readOnly`: the server marks the tool as not changing its environment (`readOnlyHint`). */
  tools: { name: string; description: string; inputSchema: Record<string, unknown>; readOnly: boolean }[]
  instructions: string
  closing?: boolean | undefined
  /** The client still connecting, so turning the server off ends the attempt. */
  pending?: Client | undefined
}

/** Waits that tests shorten. */
export interface McpTimings {
  /** Wait for an HTTP server to connect and list its tools, both transports together. */
  connectMs: number
  /** An HTTP+SSE stream that drops later than this after connecting is reopened; one that drops sooner is an error. */
  sseSettleMs: number
}

const DEFAULT_TIMINGS: McpTimings = { connectMs: CONNECT_TIMEOUT_MS, sseSettleMs: 10_000 }

/** Keeps one connection per enabled server. */
export class McpManager {
  private readonly connections = new Map<string, Connection>()
  private readonly listeners = new Set<() => void>()

  /** @param log Diagnostic sink. */
  /**
   * @param log Diagnostics.
   * @param request Request function for HTTP servers, honoring the proxy settings.
   * @param timings Waits for HTTP servers.
   */
  constructor(private readonly log: (message: string) => void, private readonly request: typeof globalThis.fetch = globalThis.fetch,
    private readonly timings: McpTimings = DEFAULT_TIMINGS) {}

  /**
   * Observe status changes.
   * @param listener Called after a connection changes state.
   * @returns A function that removes the listener.
   */
  onChange(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /**
   * Bring connections in line with the configured servers.
   * @param servers Servers from Settings.
   */
  sync(servers: readonly McpServerConfig[]): void {
    const wanted = new Map(servers.map(server => [server.name, server]))
    for (const [name, connection] of this.connections) {
      const next = wanted.get(name)
      if (next === undefined || !next.enabled || JSON.stringify(next) !== JSON.stringify(connection.config)) {
        void this.disconnect(connection)
        this.connections.delete(name)
      }
    }
    for (const server of servers) {
      if (this.connections.has(server.name)) continue
      const connection: Connection = { config: server, state: server.enabled ? 'connecting' : 'disabled', tools: [], instructions: '' }
      this.connections.set(server.name, connection)
      if (server.enabled) void this.connect(connection)
    }
    this.notify()
  }

  /** Connect again the streamable-HTTP servers that failed, for example after the proxy settings changed. */
  retryWebServers(): void {
    let retried = false
    for (const connection of this.connections.values()) {
      if (connection.config.transport !== 'streamable-http' || connection.state !== 'error') continue
      connection.state = 'connecting'
      connection.error = undefined
      retried = true
      void this.connect(connection)
    }
    if (retried) this.notify()
  }

  /** @returns Live state of every configured server. */
  status(): McpServerStatus[] {
    return [...this.connections.values()].map(connection => ({
      name: connection.config.name,
      state: connection.state,
      ...(connection.error === undefined ? {} : { error: connection.error }),
      tools: connection.tools.map(tool => ({
        name: tool.name, description: tool.description,
        enabled: connection.config.tools.length === 0 || connection.config.tools.includes(tool.name) || connection.config.tools.includes(mcpToolName(connection.config.name, tool.name)),
      })),
    }))
  }

  /** @returns Instructions of connected servers, for the system prompt. */
  instructions(): { server: string; text: string }[] {
    return [...this.connections.values()].filter(connection => connection.state === 'ready' && connection.instructions !== '')
      .map(connection => ({ server: connection.config.name, text: connection.instructions }))
  }

  /** @returns Model-facing names of connected tools their server marks as read-only. */
  readOnlyTools(): Set<string> {
    const names = new Set<string>()
    for (const connection of this.connections.values()) {
      if (connection.state !== 'ready') continue
      for (const tool of connection.tools) if (tool.readOnly) names.add(mcpToolName(connection.config.name, tool.name))
    }
    return names
  }

  /**
   * @param name A model-facing MCP tool name.
   * @returns Whether its server is connected and Settings still allows the tool; a run checks this before each call.
   */
  allows(name: string): boolean {
    for (const connection of this.connections.values()) {
      if (connection.state !== 'ready' || connection.client === undefined) continue
      for (const tool of connection.tools) {
        if (mcpToolName(connection.config.name, tool.name) !== name) continue
        return connection.config.tools.length === 0 || connection.config.tools.includes(tool.name) || connection.config.tools.includes(name)
      }
    }
    return false
  }

  /** @returns Allowed tools of connected servers as agent tools. */
  tools(): AgentTool[] {
    const tools: AgentTool[] = []
    for (const connection of this.connections.values()) {
      const client = connection.client
      if (connection.state !== 'ready' || client === undefined) continue
      const server = connection.config.name
      for (const tool of connection.tools) {
        const name = mcpToolName(server, tool.name)
        const allowed = connection.config.tools.length === 0 || connection.config.tools.includes(tool.name) || connection.config.tools.includes(name)
        if (!allowed) continue
        tools.push({
          name,
          label: `${server}: ${tool.name}`,
          description: tool.description || `${tool.name} from MCP server ${server}`,
          parameters: tool.inputSchema as unknown as TSchema,
          executionMode: 'sequential',
          async execute(_id, params, signal) {
            // A tool that reports progress keeps running past the timeout, up to the total limit.
            const result = await client.callTool({ name: tool.name, arguments: params as Record<string, unknown> }, undefined, {
              timeout: CALL_TIMEOUT_MS, onprogress: () => undefined, resetTimeoutOnProgress: true, maxTotalTimeout: CALL_TOTAL_TIMEOUT_MS,
              ...(signal === undefined ? {} : { signal }),
            })
            const content: (TextContent | ImageContent)[] = []
            for (const block of Array.isArray(result.content) ? result.content : []) {
              if (block.type === 'text' && typeof block.text === 'string') content.push({ type: 'text', text: block.text })
              else if (block.type === 'image' && typeof block.data === 'string' && typeof block.mimeType === 'string') content.push({ type: 'image', data: block.data, mimeType: block.mimeType })
              else if (block.type === 'resource' && block.resource !== null && typeof block.resource === 'object' && 'text' in block.resource && typeof block.resource.text === 'string') content.push({ type: 'text', text: block.resource.text })
            }
            if (content.length === 0 && result.structuredContent !== undefined) content.push({ type: 'text', text: JSON.stringify(result.structuredContent, null, 2) })
            if (content.length === 0) content.push({ type: 'text', text: '(no output)' })
            return { content, details: { server, tool: tool.name }, ...(result.isError === true ? { isError: true } : {}) }
          },
        })
      }
    }
    return tools
  }

  /** Close every connection. */
  async close(): Promise<void> {
    await Promise.allSettled([...this.connections.values()].map(connection => this.disconnect(connection)))
    this.connections.clear()
  }

  private async connect(connection: Connection): Promise<void> {
    const { config } = connection
    let client = new Client({ name: 'Rainy', version: '2' })
    connection.pending = client
    let sse = false
    /** The server's latest stderr, which says why a command that exits at once did not start. */
    let stderr = ''
    try {
      const http = { fetch: this.request, ...(config.headers === undefined ? {} : { requestInit: { headers: config.headers } }) }
      const transport = config.transport === 'stdio'
        ? new StdioClientTransport({ command: config.command!, args: config.args ?? [], env: { ...scrubbedEnv(), ...config.env }, stderr: 'pipe' })
        : new StreamableHTTPClientTransport(new URL(config.url!), http)
      if (transport instanceof StdioClientTransport) {
        transport.stderr?.on('data', (chunk: Buffer) => {
          const text = consoleText(chunk)
          stderr = (stderr + text).slice(-600)
          this.log(`[mcp:${config.name}] ${text.trimEnd()}`)
        })
      }
      const timeout = config.transport === 'stdio' ? STDIO_CONNECT_TIMEOUT_MS : this.timings.connectMs
      const deadline = Date.now() + timeout
      try {
        await client.connect(transport, { timeout })
      } catch (error) {
        // A server from before Streamable HTTP refuses its POST with a 4xx status and speaks the older HTTP+SSE transport at
        // the same address, as many `/sse` addresses in tutorials do. Authorization failures are reported as they are.
        if (!(error instanceof StreamableHTTPError) || error.code === undefined || error.code < 400 || error.code >= 500 || error.code === 401 || error.code === 403) throw error
        this.log(`[mcp:${config.name}] ${error.message}; trying HTTP+SSE`)
        await client.close().catch(() => undefined)
        client = await this.connectSse(connection, http, Math.max(5_000, deadline - Date.now()), error)
        sse = true
      }
      const listed = await client.listTools(undefined, { timeout })
      if (connection.closing) { await client.close(); return }
      connection.client = client
      connection.tools = listed.tools.map(tool => ({
        name: tool.name, description: tool.description ?? '', inputSchema: tool.inputSchema as Record<string, unknown>, readOnly: tool.annotations?.readOnlyHint === true,
      }))
      connection.instructions = client.getInstructions() ?? ''
      connection.state = 'ready'
      connection.error = undefined
      const connectedAt = Date.now()
      // An HTTP+SSE stream that drops is reopened by the event source on a new session that was never initialized, so
      // the client is closed instead and the server connected again.
      if (sse) client.onerror = (failure) => { if (failure instanceof SseError && connection.client === client) void client.close() }
      client.onclose = () => {
        if (connection.closing || connection.client !== client) return
        connection.client = undefined
        if (sse && Date.now() - connectedAt > this.timings.sseSettleMs) {
          this.log(`[mcp:${config.name}] the HTTP+SSE stream closed; connecting again`)
          connection.state = 'connecting'
          connection.error = undefined
          this.notify()
          void this.connect(connection)
          return
        }
        connection.state = 'error'
        connection.error = localized({ zh: 'MCP 服务器断开了连接。', en: 'The server closed the connection.' })
        this.notify()
      }
    } catch (error) {
      connection.state = 'error'
      const reason = error instanceof Error ? error.message : String(error)
      const output = stderr.trim().split(/\r?\n/u).slice(-3).join(' ').trim()
      connection.error = config.transport !== 'stdio' ? reason
        : /\bENOENT\b/u.test(reason)
          ? localized({ zh: `找不到启动命令 ${config.command ?? ''}。请检查命令名称，或填写完整路径。`, en: `The command ${config.command ?? ''} was not found. Check its name, or enter its full path.` })
          : /connection closed/iu.test(reason)
            ? localized({ zh: `启动命令运行后立即退出了。${output === '' ? '请检查命令、参数和环境变量。' : `输出：${output}`}`,
              en: `The command exited right after starting. ${output === '' ? 'Check the command, arguments and environment variables.' : `Output: ${output}`}` })
            : reason
      this.log(`[mcp:${config.name}] ${reason}`)
      await client.close().catch(() => undefined)
    } finally {
      if (connection.pending === client) connection.pending = undefined
    }
    this.notify()
  }

  /**
   * Connect over the older HTTP+SSE transport, within what is left of the connect timeout: its start waits for the
   * server's `endpoint` event with no timeout of its own.
   * @param streamable Why Streamable HTTP failed, reported when the address is no SSE endpoint either.
   * @returns The connected client.
   */
  private async connectSse(connection: Connection, http: { fetch: typeof globalThis.fetch; requestInit?: RequestInit }, timeout: number,
    streamable: StreamableHTTPError): Promise<Client> {
    const { config } = connection
    const client = new Client({ name: 'Rainy', version: '2' })
    connection.pending = client
    let timer: ReturnType<typeof setTimeout> | undefined
    try {
      if (connection.closing) throw streamable
      await Promise.race([
        client.connect(new SSEClientTransport(new URL(config.url!), http), { timeout }),
        new Promise<never>((_resolve, reject) => {
          timer = setTimeout(() => {
            reject(new Error(localized({ zh: 'HTTP+SSE 连接超时：服务器没有发来会话地址。', en: 'The HTTP+SSE connection timed out: the server sent no session address.' })))
          }, timeout)
        }),
      ])
      this.log(`[mcp:${config.name}] connected over HTTP+SSE`)
      return client
    } catch (error) {
      await client.close().catch(() => undefined)
      this.log(`[mcp:${config.name}] HTTP+SSE: ${error instanceof Error ? error.message : String(error)}`)
      // An address that is no SSE endpoint either adds nothing to the first error; anything else, such as a refused token,
      // a session address on another site or a timeout, is the real cause.
      const notSse = error instanceof SseError && (error.code === 404 || error.code === 405 || /content.type/iu.test(error.message))
      throw notSse || error === streamable ? streamable : error
    } finally {
      clearTimeout(timer)
      if (connection.pending === client) connection.pending = undefined
    }
  }

  private async disconnect(connection: Connection): Promise<void> {
    connection.closing = true
    await Promise.all([connection.client, connection.pending].map(client => client?.close().catch(() => undefined)))
    connection.client = undefined
  }

  private notify(): void {
    for (const listener of this.listeners) listener()
  }
}
