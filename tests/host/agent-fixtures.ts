/** Test doubles for the agent side: a scripted OpenAI-compatible server and a Host home. */
import { createServer, type Server } from 'node:http'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { createProjectRegistry } from '../../src/shared/project-registry.ts'
import type { HostEvents, HostMethod, MethodParams, MethodResult, PermissionMode } from '../../src/shared/rpc.ts'
import { Activity } from '../../src/host/activity.ts'
import { createAgent, type AgentService } from '../../src/host/agent/index.ts'
import type { HostEnvironment } from '../../src/host/env.ts'
import { Projects } from '../../src/host/projects.ts'
import { RpcHub } from '../../src/host/rpc.ts'
import type { RuntimeService } from '../../src/host/runtime/index.ts'
import { Settings } from '../../src/host/settings.ts'

/** One scripted assistant reply. */
export type ScriptedReply =
  /** Optional reasoning streams first (as `reasoning_content`), then `pauseMs` passes before the text. */
  | { text: string; thinking?: string | undefined; pauseMs?: number | undefined }
  | { toolCalls: { name: string; arguments: Record<string, unknown> }[] }
  | { status: number; body: string }
  /** Starts streaming this text, then drops the connection. */
  | { drop: string }

/** A local `/v1/chat/completions` server that answers requests with scripted replies in order. */
export class FakeOpenAI {
  readonly requests: Record<string, unknown>[] = []
  private server: Server | undefined
  private port = 0

  /** @param replies Replies in request order. */
  constructor(private readonly replies: ScriptedReply[]) {}

  /** @returns The base URL including `/v1`. */
  get baseURL(): string { return `http://127.0.0.1:${this.port}/v1` }

  async start(): Promise<void> {
    this.server = createServer((request, response) => {
      const chunks: Buffer[] = []
      request.on('data', (chunk: Buffer) => { chunks.push(chunk) })
      request.on('end', () => {
        if (request.url?.endsWith('/models')) {
          response.writeHead(200, { 'content-type': 'application/json' })
          response.end(JSON.stringify({ data: [{ id: 'fake-model', context_length: 32768 }] }))
          return
        }
        this.requests.push(JSON.parse(Buffer.concat(chunks).toString('utf8')) as Record<string, unknown>)
        const reply = this.replies.shift() ?? { text: 'no more scripted replies' }
        if ('status' in reply) { response.writeHead(reply.status, { 'content-type': 'application/json' }); response.end(reply.body); return }
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        const send = (data: unknown): void => { response.write(`data: ${JSON.stringify(data)}\n\n`) }
        const base = { id: 'chatcmpl-1', object: 'chat.completion.chunk', created: 1, model: 'fake-model' }
        if ('drop' in reply) {
          send({ ...base, choices: [{ index: 0, delta: { role: 'assistant', content: reply.drop } }] })
          setTimeout(() => { response.destroy() }, 20)
          return
        }
        if ('text' in reply) {
          for (const part of reply.thinking?.match(/.{1,8}/gs) ?? []) send({ ...base, choices: [{ index: 0, delta: { role: 'assistant', reasoning_content: part } }] })
          const finish = (): void => {
            for (const part of reply.text.match(/.{1,8}/gs) ?? ['']) send({ ...base, choices: [{ index: 0, delta: { role: 'assistant', content: part } }] })
            send({ ...base, choices: [{ index: 0, delta: {}, finish_reason: 'stop' }], usage: { prompt_tokens: 100, completion_tokens: 10, total_tokens: 110 } })
            response.end('data: [DONE]\n\n')
          }
          if (reply.pauseMs === undefined) finish()
          else setTimeout(finish, reply.pauseMs)
          return
        }
        reply.toolCalls.forEach((call, index) => {
          send({ ...base, choices: [{ index: 0, delta: { role: 'assistant', tool_calls: [{ index, id: `call_${index}_${this.requests.length}`, type: 'function', function: { name: call.name, arguments: JSON.stringify(call.arguments) } }] } }] })
        })
        send({ ...base, choices: [{ index: 0, delta: {}, finish_reason: 'tool_calls' }], usage: { prompt_tokens: 100, completion_tokens: 20, total_tokens: 120 } })
        response.end('data: [DONE]\n\n')
      })
    })
    await new Promise<void>(resolve => { this.server!.listen(0, '127.0.0.1', resolve) })
    const address = this.server.address()
    if (address === null || typeof address === 'string') throw new Error('no address')
    this.port = address.port
  }

  async stop(): Promise<void> {
    this.server?.closeAllConnections()
    await new Promise<void>(resolve => { this.server?.close(() => { resolve() }) ?? resolve() })
  }
}

/** A local Anthropic `/v1/messages` server that answers every request with one streamed text reply. */
export class FakeAnthropic {
  readonly requests: Record<string, unknown>[] = []
  private server: Server | undefined
  private port = 0

  /** @param replies Reply texts in request order. */
  constructor(private readonly replies: string[]) {}

  /** @returns The base URL, without `/v1`. */
  get baseURL(): string { return `http://127.0.0.1:${this.port}` }

  async start(): Promise<void> {
    this.server = createServer((request, response) => {
      const chunks: Buffer[] = []
      request.on('data', (chunk: Buffer) => { chunks.push(chunk) })
      request.on('end', () => {
        this.requests.push(JSON.parse(Buffer.concat(chunks).toString('utf8')) as Record<string, unknown>)
        const text = this.replies.shift() ?? 'no more scripted replies'
        response.writeHead(200, { 'content-type': 'text/event-stream' })
        const send = (type: string, data: Record<string, unknown>): void => { response.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`) }
        send('message_start', { message: {
          id: `msg_${this.requests.length}`, type: 'message', role: 'assistant', model: 'claude', content: [], stop_reason: null,
          usage: { input_tokens: 100, output_tokens: 1, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 },
        } })
        send('content_block_start', { index: 0, content_block: { type: 'text', text: '' } })
        send('content_block_delta', { index: 0, delta: { type: 'text_delta', text } })
        send('content_block_stop', { index: 0 })
        send('message_delta', { delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 10 } })
        send('message_stop', {})
        response.end()
      })
    })
    await new Promise<void>(resolve => { this.server!.listen(0, '127.0.0.1', resolve) })
    const address = this.server.address()
    if (address === null || typeof address === 'string') throw new Error('no address')
    this.port = address.port
  }

  async stop(): Promise<void> {
    this.server?.closeAllConnections()
    await new Promise<void>(resolve => { this.server?.close(() => { resolve() }) ?? resolve() })
  }
}

/**
 * A throwaway Host home.
 * @returns The environment and a cleanup function.
 */
export async function tempHost(): Promise<{ env: HostEnvironment; dir: string; cleanup(): Promise<void> }> {
  const dir = await mkdtemp(join(tmpdir(), 'rainy-agent-test-'))
  const env: HostEnvironment = {
    version: '0.0.0-test', home: join(dir, 'home'), appRoot: dir, resources: join(dir, 'resources'), platform: process.platform === 'win32' ? 'win32' : 'linux',
    executionTargetId: 'test-target', carrierStateRoot: join(dir, 'carrier'), toolchainRoot: join(dir, 'components'), tmp: join(dir, 'tmp'), port: 0,
  }
  return { env, dir, cleanup: () => rm(dir, { recursive: true, force: true }) }
}

const runtime: RuntimeService = {
  handle: () => Promise.reject(new Error('not used')),
  resolveDirectory: () => ({ environment: {}, executables: {} }),
  resolveWorkspace: () => ({ environment: {}, executables: {} }),
  onSelectionChange: () => () => undefined,
}

/** An agent side over a temp Host home with one project, driven through RPC dispatch. */
export interface Harness {
  call<M extends HostMethod>(method: M, params: MethodParams<M>): Promise<MethodResult<M>>
  events: { event: keyof HostEvents; data: unknown }[]
  workspaceId: WorkspaceId
  project: string
  agent: AgentService
  home: string
  settings: Settings
}

/**
 * Build an agent side whose model is `server`. Chats run tools without approval unless a test asks for another mode.
 * @param server Fake model server.
 * @param cleanups Collects teardown steps.
 * @param mode Mode of new chats.
 * @returns The harness.
 */
export async function harness(server: { baseURL: string }, cleanups: (() => Promise<void>)[], mode: PermissionMode = 'auto'): Promise<Harness> {
  const { env, dir, cleanup } = await tempHost()
  cleanups.push(cleanup)
  const project = join(dir, 'project')
  await mkdir(project, { recursive: true })
  const settings = new Settings(env.home, {})
  await settings.load()
  const projects = new Projects(join(env.home, 'projects.json'))
  await projects.load()
  const opened = await projects.open(project)
  const rpc = new RpcHub()
  const events: Harness['events'] = []
  const originalEmit = rpc.emit.bind(rpc)
  rpc.emit = ((event: keyof HostEvents, data: never) => { events.push({ event, data }); originalEmit(event, data) }) as typeof rpc.emit
  const agent = await createAgent({
    env, settings, projects, registry: createProjectRegistry({ root: env.carrierStateRoot, targetId: env.executionTargetId }),
    runtime, rpc, activity: new Activity(), log: () => undefined,
  })
  cleanups.push(() => agent.close())
  let id = 1
  const call = async <M extends HostMethod>(method: M, params: MethodParams<M>): Promise<MethodResult<M>> => {
    const response = await rpc.dispatch({ id: id++, method, params })
    if (response.error !== undefined) throw new Error(`${response.error.code}: ${response.error.message}`)
    return response.result as MethodResult<M>
  }
  await call('models.configure', { provider: 'fake', baseURL: server.baseURL, model: 'fake-model', contextWindow: 32768, local: true, api: 'openai-completions' })
  await call('security.setDefault', { mode })
  return { call, events, workspaceId: opened.id, project: opened.path, agent, home: env.home, settings }
}

/** Wait until a chat stops running. */
export async function idle(h: Harness, sessionId: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt++) {
    const snapshot = await h.call('sessions.get', { sessionId })
    if (snapshot.summary.status !== 'running' && snapshot.summary.status !== 'compacting') return
    await new Promise(resolve => setTimeout(resolve, 25))
  }
  throw new Error('chat did not finish')
}
