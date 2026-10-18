/** MCP servers configured in Settings: connections, their tools as agent tools, and their instructions. */
import { createHash } from 'node:crypto'
import { Client } from '@modelcontextprotocol/sdk/client/index.js'
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js'
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js'
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
}

/** Keeps one connection per enabled server. */
export class McpManager {
  private readonly connections = new Map<string, Connection>()
  private readonly listeners = new Set<() => void>()

  /** @param log Diagnostic sink. */
  /**
   * @param log Diagnostics.
   * @param request Request function for HTTP servers, honoring the proxy settings.
   */
  constructor(private readonly log: (message: string) => void, private readonly request: typeof globalThis.fetch = globalThis.fetch) {}

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
    const client = new Client({ name: 'Rainy', version: '2' })
    /** The server's latest stderr, which says why a command that exits at once did not start. */
    let stderr = ''
    try {
      const transport = config.transport === 'stdio'
        ? new StdioClientTransport({ command: config.command!, args: config.args ?? [], env: { ...scrubbedEnv(), ...config.env }, stderr: 'pipe' })
        : new StreamableHTTPClientTransport(new URL(config.url!), { fetch: this.request, ...(config.headers === undefined ? {} : { requestInit: { headers: config.headers } }) })
      if (transport instanceof StdioClientTransport) {
        transport.stderr?.on('data', (chunk: Buffer) => {
          const text = consoleText(chunk)
          stderr = (stderr + text).slice(-600)
          this.log(`[mcp:${config.name}] ${text.trimEnd()}`)
        })
      }
      await client.connect(transport, { timeout: CONNECT_TIMEOUT_MS })
      const listed = await client.listTools(undefined, { timeout: CONNECT_TIMEOUT_MS })
      if (connection.closing) { await client.close(); return }
      connection.client = client
      connection.tools = listed.tools.map(tool => ({
        name: tool.name, description: tool.description ?? '', inputSchema: tool.inputSchema as Record<string, unknown>, readOnly: tool.annotations?.readOnlyHint === true,
      }))
      connection.instructions = client.getInstructions() ?? ''
      connection.state = 'ready'
      connection.error = undefined
      client.onclose = () => {
        if (connection.closing) return
        connection.state = 'error'
        connection.error = localized({ zh: 'MCP 服务器断开了连接。', en: 'The server closed the connection.' })
        connection.client = undefined
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
    }
    this.notify()
  }

  private async disconnect(connection: Connection): Promise<void> {
    connection.closing = true
    await connection.client?.close().catch(() => undefined)
    connection.client = undefined
  }

  private notify(): void {
    for (const listener of this.listeners) listener()
  }
}
