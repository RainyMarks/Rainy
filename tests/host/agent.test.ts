import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { estimateText, resolveBudget } from '../../src/shared/budget.ts'
import { brandString } from '../../src/shared/brand.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { createProjectRegistry } from '../../src/shared/project-registry.ts'
import type { HostEvents, MethodParams, MethodResult, HostMethod, TranscriptEntry } from '../../src/shared/rpc.ts'
import { Activity } from '../../src/host/activity.ts'
import { SHARED_CACHE_WINDOW_MS } from '../../src/host/agent/compaction.ts'
import { createAgent, type AgentService } from '../../src/host/agent/index.ts'
import { Projects } from '../../src/host/projects.ts'
import { RpcHub } from '../../src/host/rpc.ts'
import type { RuntimeService } from '../../src/host/runtime/index.ts'
import { Settings } from '../../src/host/settings.ts'
import { FakeAnthropic, FakeOpenAI, tempHost, type ScriptedReply } from './agent-fixtures.ts'

const runtime: RuntimeService = {
  handle: () => Promise.reject(new Error('not used')),
  resolveDirectory: () => ({ environment: {}, executables: {} }),
  resolveWorkspace: () => ({ environment: {}, executables: {} }),
}

interface Harness {
  call<M extends HostMethod>(method: M, params: MethodParams<M>): Promise<MethodResult<M>>
  events: { event: keyof HostEvents; data: unknown }[]
  workspaceId: WorkspaceId
  project: string
  agent: AgentService
  home: string
}

async function harness(server: { baseURL: string }, cleanups: (() => Promise<void>)[]): Promise<Harness> {
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
  return { call, events, workspaceId: opened.id, project: opened.path, agent, home: env.home }
}

async function idle(h: Harness, sessionId: string): Promise<void> {
  for (let attempt = 0; attempt < 200; attempt++) {
    const snapshot = await h.call('sessions.get', { sessionId })
    if (snapshot.summary.status !== 'running' && snapshot.summary.status !== 'compacting') return
    await new Promise(resolve => setTimeout(resolve, 25))
  }
  throw new Error('chat did not finish')
}

describe('agent', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI
  beforeEach(() => { cleanups.length = 0 })
  afterEach(async () => {
    for (const cleanup of cleanups.reverse()) await cleanup()
    await server?.stop()
  })

  it('runs a tool call and persists the transcript', async () => {
    server = new FakeOpenAI([
      { toolCalls: [{ name: process.platform === 'win32' ? 'pwsh' : 'bash', arguments: { command: 'echo rainy-hello', description: 'Print a greeting' } }] },
      { text: 'All done.' },
    ])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    const sent = await h.call('chat.send', { sessionId: chat.id, text: 'say hello please' })
    expect(sent.queued).toBe(false)
    await idle(h, chat.id)
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    const kinds = snapshot.entries.map(entry => entry.kind)
    expect(kinds).toEqual(['user', 'assistant', 'toolResult', 'assistant', 'turn'])
    const result = snapshot.entries[2] as Extract<TranscriptEntry, { kind: 'toolResult' }>
    expect(result.isError).toBe(false)
    expect(JSON.stringify(result.content)).toContain('rainy-hello')
    expect(snapshot.summary.title).toBe('say hello please')
    expect(snapshot.summary.cwd).toBe(h.project)
    // The second request carries the tool result back to the model.
    expect(JSON.stringify(server.requests[1])).toContain('rainy-hello')
    expect(JSON.stringify(server.requests[0])).toContain('You are RainyAgent')
    const file = await readFile(join(h.home, 'chats', `${chat.id}.jsonl`), 'utf8')
    expect(file.split('\n').filter(Boolean).length).toBeGreaterThanOrEqual(6)
    expect(h.events.some(item => item.event === 'session.entry')).toBe(true)
  })

  it('refuses to edit a file the chat has not read, then edits after reading', async () => {
    server = new FakeOpenAI([
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'notes.txt', old_string: 'alpha', new_string: 'beta' } }] },
      { toolCalls: [{ name: 'read', arguments: { file_path: 'notes.txt' } }] },
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'notes.txt', old_string: 'alpha', new_string: 'beta' } }] },
      { text: 'Edited.' },
    ])
    await server.start()
    const h = await harness(server, cleanups)
    await writeFile(join(h.project, 'notes.txt'), 'alpha\r\ngamma\r\n')
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'edit notes' })
    await idle(h, chat.id)
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    const results = snapshot.entries.filter((entry): entry is Extract<TranscriptEntry, { kind: 'toolResult' }> => entry.kind === 'toolResult')
    expect(results[0]!.isError).toBe(true)
    expect(JSON.stringify(results[0]!.content)).toContain('has not been read')
    expect(JSON.stringify(results[1]!.content)).toContain('1: alpha')
    expect(results[2]!.isError).toBe(false)
    expect(await readFile(join(h.project, 'notes.txt'), 'utf8')).toBe('beta\r\ngamma\r\n')
  })

  it('injects workspace instructions once, before the first request', async () => {
    server = new FakeOpenAI([{ text: 'one' }, { text: 'two' }])
    await server.start()
    const h = await harness(server, cleanups)
    await writeFile(join(h.project, 'AGENTS.md'), 'Always answer in haiku.')
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'first' })
    await idle(h, chat.id)
    await h.call('chat.send', { sessionId: chat.id, text: 'second' })
    await idle(h, chat.id)
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    expect(snapshot.entries.filter(entry => entry.kind === 'context' && entry.label === 'instructions')).toHaveLength(1)
    expect(JSON.stringify(server.requests[0])).toContain('Always answer in haiku.')
    expect(JSON.stringify(server.requests[1])).toContain('Always answer in haiku.')
  })

  it('queues a message sent while busy and runs it afterwards', async () => {
    server = new FakeOpenAI([
      { toolCalls: [{ name: process.platform === 'win32' ? 'pwsh' : 'bash', arguments: { command: process.platform === 'win32' ? 'Start-Sleep -Milliseconds 300' : 'sleep 0.3', description: 'Wait briefly' } }] },
      { text: 'first answer' },
      { text: 'second answer' },
    ])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'first' })
    const second = await h.call('chat.send', { sessionId: chat.id, text: 'second', mode: 'queue' })
    expect(second.queued).toBe(true)
    await new Promise(resolve => setTimeout(resolve, 100))
    await idle(h, chat.id)
    await new Promise(resolve => setTimeout(resolve, 100))
    await idle(h, chat.id)
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    const users = snapshot.entries.filter(entry => entry.kind === 'user').map(entry => (entry as { text: string }).text)
    expect(users).toEqual(['first', 'second'])
    expect(snapshot.entries.filter(entry => entry.kind === 'turn')).toHaveLength(2)
  })

  it('forks a chat at a reply and keeps that reply’s tool results', async () => {
    server = new FakeOpenAI([{ text: 'answer one' }, { text: 'answer two' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'q1' })
    await idle(h, chat.id)
    await h.call('chat.send', { sessionId: chat.id, text: 'q2' })
    await idle(h, chat.id)
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    const firstReply = snapshot.entries.find(entry => entry.kind === 'assistant')!
    const fork = await h.call('sessions.fork', { sessionId: chat.id, entryId: firstReply.id })
    const forked = await h.call('sessions.get', { sessionId: fork.id })
    expect(forked.entries.map(entry => entry.kind)).toEqual(['user', 'assistant'])
    expect(fork.parent).toEqual({ sessionId: chat.id, entryId: firstReply.id })
    const list = await h.call('sessions.list', { workspaceId: h.workspaceId })
    expect(list.map(item => item.id)).toContain(fork.id)
  })

  it('reports provider errors on the assistant entry and recovers on the next message', async () => {
    server = new FakeOpenAI([{ status: 401, body: '{"error":{"message":"bad key"}}' }, { text: 'recovered' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'hi' })
    await idle(h, chat.id)
    let snapshot = await h.call('sessions.get', { sessionId: chat.id })
    const failed = snapshot.entries.find(entry => entry.kind === 'assistant') as Extract<TranscriptEntry, { kind: 'assistant' }>
    expect(failed.message.stopReason).toBe('error')
    await h.call('chat.send', { sessionId: chat.id, text: 'again' })
    await idle(h, chat.id)
    snapshot = await h.call('sessions.get', { sessionId: chat.id })
    const last = snapshot.entries.filter(entry => entry.kind === 'assistant').at(-1) as Extract<TranscriptEntry, { kind: 'assistant' }>
    expect(JSON.stringify(last.message.content)).toContain('recovered')
    // The failed reply is not replayed to the provider.
    expect(JSON.stringify(server.requests[1])).not.toContain('bad key')
  })

  /** The request body without its messages, and its messages. */
  const split = (body: Record<string, unknown> | undefined): { rest: Record<string, unknown>; messages: unknown[] } => {
    const { messages, ...rest } = body ?? {}
    return { rest, messages: messages as unknown[] }
  }
  const compactions = async (h: Harness, sessionId: string) => (await h.call('sessions.get', { sessionId })).entries
    .filter((entry): entry is Extract<TranscriptEntry, { kind: 'compaction' }> => entry.kind === 'compaction')

  it('compacts on request by repeating the previous request unchanged plus one instruction', async () => {
    const long = 'x'.repeat(4000)
    server = new FakeOpenAI([{ text: long }, { text: long }, { text: long }, { text: 'SUMMARY: earlier work' }, { text: 'after compaction' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    for (const text of ['a', 'b', 'c']) {
      await h.call('chat.send', { sessionId: chat.id, text })
      await idle(h, chat.id)
    }
    const result = await h.call('chat.compact', { sessionId: chat.id })
    expect(result.message).toMatch(/^Compacted/)
    // Same model, tools, system prompt, history and options; no tool_choice; only the reply and the instruction added.
    const previous = split(server.requests[2])
    const shared = split(server.requests[3])
    expect(shared.rest).toEqual(previous.rest)
    expect(shared.rest.tool_choice).toBeUndefined()
    expect(shared.messages.slice(0, previous.messages.length)).toEqual(previous.messages)
    expect(shared.messages).toHaveLength(previous.messages.length + 2)
    expect(JSON.stringify(shared.messages.at(-1))).toContain('do not call any tools')
    const [entry] = await compactions(h, chat.id)
    expect(entry?.request).toMatchObject({ mode: 'shared', usage: { input: 100, output: 10, cacheRead: 0, cacheWrite: 0 } })

    await h.call('chat.send', { sessionId: chat.id, text: 'd' })
    await idle(h, chat.id)
    const after = split(server.requests.at(-1))
    expect(JSON.stringify(after.messages)).toContain('SUMMARY: earlier work')
    expect(JSON.stringify(after.messages)).toContain('compacted-summary')
    expect(after.rest.tools).toEqual(previous.rest.tools)
    expect(after.messages[0]).toEqual(previous.messages[0])
  })

  it('compacts between tool steps by repeating the interrupted request', async () => {
    const replies = ['x'.repeat(120000), 'x'.repeat(60000)]
    const script: ScriptedReply[] = replies.map(text => ({ text }))
    server = new FakeOpenAI(script)
    await server.start()
    const h = await harness(server, cleanups)
    await h.call('models.configure', { provider: 'fake', baseURL: server.baseURL, model: 'fake-model', contextWindow: 131072, local: true, api: 'openai-completions' })
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    for (const text of ['a', 'b']) {
      await h.call('chat.send', { sessionId: chat.id, text })
      await idle(h, chat.id)
    }
    // Grow the context past the compaction threshold with one tool call, leaving room for the shared request.
    const budget = resolveBudget(131072)
    // The last request holds everything but its own reply.
    const used = estimateText(JSON.stringify(server.requests.at(-1))) + estimateText(replies.at(-1)!)
    const comment = 'y'.repeat(Math.max(0, budget.compactAt + 4000 - used) * 3)
    script.push({ toolCalls: [{ name: process.platform === 'win32' ? 'pwsh' : 'bash', arguments: { command: `echo step # ${comment}`, description: 'Step' } }] },
      { text: 'SUMMARY: mid-run' }, { text: 'done' })
    await h.call('chat.send', { sessionId: chat.id, text: 'c' })
    await idle(h, chat.id)
    expect(server.requests).toHaveLength(5)
    const interrupted = split(server.requests[2])
    const shared = split(server.requests[3])
    expect(shared.rest).toEqual(interrupted.rest)
    expect(shared.messages.slice(0, interrupted.messages.length)).toEqual(interrupted.messages)
    // The tool call, its result and the instruction follow the interrupted request.
    expect(shared.messages.slice(interrupted.messages.length).map(message => (message as { role: string }).role)).toEqual(['assistant', 'tool', 'user'])
    const resumed = JSON.stringify(server.requests[4])
    expect(resumed).toContain('SUMMARY: mid-run')
    expect(resumed).toContain('echo step')
    const [entry] = await compactions(h, chat.id)
    expect(entry?.trigger).toBe('auto')
    expect(entry?.request?.mode).toBe('shared')
    const turn = (await h.call('sessions.get', { sessionId: chat.id })).entries.filter(item => item.kind === 'turn').at(-1)
    // The tool step, the summary and the resumed answer: the turn's usage includes the summary request.
    expect(turn?.kind === 'turn' && turn.usage.input).toBe(300)
  })

  it('falls back to a separate summary request when the shared one calls a tool', async () => {
    const long = 'x'.repeat(4000)
    server = new FakeOpenAI([{ text: long }, { text: long }, { text: long },
      { toolCalls: [{ name: 'read', arguments: { file_path: 'notes.txt' } }] }, { text: 'SUMMARY: separate' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    for (const text of ['a', 'b', 'c']) {
      await h.call('chat.send', { sessionId: chat.id, text })
      await idle(h, chat.id)
    }
    await h.call('chat.compact', { sessionId: chat.id })
    const separate = split(server.requests[4])
    expect(separate.rest.tool_choice).toBe('none')
    expect(JSON.stringify(separate.messages.at(-1))).toContain('replace the conversation above')
    const [entry] = await compactions(h, chat.id)
    expect(entry?.summary).toBe('SUMMARY: separate')
    expect(entry?.request).toMatchObject({ mode: 'separate', usage: { input: 200, output: 30, cacheRead: 0, cacheWrite: 0 } })
    expect(JSON.stringify((await h.call('sessions.get', { sessionId: chat.id })).entries)).not.toContain('notes.txt')
  })

  it('reads the previous Claude request’s cache entry and writes none for the summary request', async () => {
    const claude = new FakeAnthropic(['x'.repeat(4000), 'x'.repeat(4000), 'x'.repeat(4000), 'SUMMARY: claude'])
    await claude.start()
    cleanups.push(() => claude.stop())
    const h = await harness(claude, cleanups)
    await h.call('models.configure', { provider: 'anthropic', baseURL: claude.baseURL, model: 'claude-opus-5-5', contextWindow: 32768, local: false, api: 'anthropic-messages', apiKey: 'test-key' })
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    for (const text of ['a', 'b', 'c']) {
      await h.call('chat.send', { sessionId: chat.id, text })
      await idle(h, chat.id)
    }
    await h.call('chat.compact', { sessionId: chat.id })
    expect(claude.requests).toHaveLength(4)
    const previous = split(claude.requests[2])
    const shared = split(claude.requests[3])
    // Breakpoints move between requests and are not part of the cached bytes.
    const unmarked = (value: unknown): unknown => JSON.parse(JSON.stringify(value, (key, item: unknown) => key === 'cache_control' ? undefined : item))
    expect(unmarked(shared.rest)).toEqual(unmarked(previous.rest))
    expect(shared.rest.tool_choice).toBeUndefined()
    expect(unmarked(shared.messages.slice(0, previous.messages.length))).toEqual(unmarked(previous.messages))
    const lastUser = (messages: unknown[]): number => messages.findLastIndex(message => (message as { role: string }).role === 'user')
    const marker = (message: unknown): unknown => (message as { content: { cache_control?: unknown }[] }).content.at(-1)?.cache_control
    // The previous request wrote its entry at its last user message; the summary request reads exactly there and
    // marks nothing after it, so it writes no cache.
    const written = lastUser(previous.messages)
    expect(marker(previous.messages[written])).toEqual({ type: 'ephemeral', ttl: '1h' })
    expect(marker(shared.messages[written])).toEqual({ type: 'ephemeral', ttl: '1h' })
    expect(lastUser(shared.messages)).toBeGreaterThan(previous.messages.length)
    expect(JSON.stringify(shared.messages.slice(written + 1))).not.toContain('cache_control')
    const [entry] = await compactions(h, chat.id)
    expect(entry?.request?.mode).toBe('shared')
  })

  it('sends a separate summary request when the previous request’s cache has expired', async () => {
    const long = 'x'.repeat(4000)
    server = new FakeOpenAI([{ text: long }, { text: long }, { text: long }, { text: 'SUMMARY: cold' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    for (const text of ['a', 'b', 'c']) {
      await h.call('chat.send', { sessionId: chat.id, text })
      await idle(h, chat.id)
    }
    const now = Date.now()
    const clock = vi.spyOn(Date, 'now').mockReturnValue(now + SHARED_CACHE_WINDOW_MS + 1)
    try {
      await h.call('chat.compact', { sessionId: chat.id })
    } finally {
      clock.mockRestore()
    }
    expect(server.requests).toHaveLength(4)
    expect(split(server.requests[3]).rest.tool_choice).toBe('none')
    const [entry] = await compactions(h, chat.id)
    expect(entry?.request?.mode).toBe('separate')
  })

  it('imports RainyAgent 1.x models, keys and the global prompt once', async () => {
    const { env, cleanup } = await tempHost()
    cleanups.push(cleanup)
    await mkdir(join(env.home, 'profiles', 'rainy'), { recursive: true })
    await writeFile(join(env.home, '.credentials.yaml'), 'DEEPSEEK_API_KEY:\n  kind: api-key\n  key: sk-test-123\n')
    await writeFile(join(env.home, 'profiles', 'rainy', 'cordis.patch.yml'), [
      '- id: llm-pi-ai', '  config:', '    providers:', '      rainy-deepseek:', '        displayName: DeepSeek V4.1 Flash',
      '        baseURL: https://api.deepseek.com', '        api: openai-responses', '        reasoning: max', '        defaultContextWindow: 1000000',
      '        models:', '          - id: deepseek-flash', '            contextWindow: 1000000', '            maxTokens: 393216',
      '- id: agent-default-model', '  config:', '    provider: rainy-deepseek', '    model: deepseek-flash', '    reasoningEffort: max',
      '- id: rainy-policy', '  config:', '    globalPrompt: 用中文回答',
    ].join('\n'))
    const settings = new Settings(env.home, {})
    await settings.load()
    const data = settings.get()
    expect(data.models).toEqual([expect.objectContaining({ provider: 'rainy-deepseek', model: 'deepseek-flash', contextWindow: 1000000, thinking: 'max' })])
    expect(data.selected).toEqual({ provider: 'rainy-deepseek', model: 'deepseek-flash', thinking: 'max' })
    expect(data.globalPrompt).toBe('用中文回答')
    expect(settings.apiKey('rainy-deepseek')).toBe('sk-test-123')
  })

  it('completes files and chats after @', async () => {
    server = new FakeOpenAI([])
    await server.start()
    const h = await harness(server, cleanups)
    await mkdir(join(h.project, 'src'), { recursive: true })
    await writeFile(join(h.project, 'src', 'solve.py'), 'print(1)')
    const items = await h.call('chat.complete', { workspaceId: h.workspaceId, query: 'solv' })
    expect(items[0]).toEqual(expect.objectContaining({ kind: 'file', insert: 'src/solve.py' }))
    const folder = await h.call('chat.complete', { workspaceId: h.workspaceId, query: 'src/' })
    expect(folder.map(item => item.insert)).toContain('src/solve.py')
  })
})

describe('ids', () => {
  it('brands strings without changing them', () => {
    expect(brandString<WorkspaceId>('abc')).toBe('abc')
  })
})
