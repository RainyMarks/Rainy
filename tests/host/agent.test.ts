import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { estimateText, resolveBudget } from '../../src/shared/budget.ts'
import { brandString } from '../../src/shared/brand.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { TranscriptEntry } from '../../src/shared/rpc.ts'
import { SHARED_CACHE_WINDOW_MS } from '../../src/host/agent/compaction.ts'
import { fallbackTitle, referenceSnapshot } from '../../src/host/agent/session.ts'
import { Settings } from '../../src/host/settings.ts'
import { FakeAnthropic, FakeOpenAI, harness, idle, tempHost, type Harness, type ScriptedReply } from './agent-fixtures.ts'

it('names a chat after the first words of its first message, marking a cut', () => {
  expect(fallbackTitle('say hello please')).toBe('say hello please')
  expect(fallbackTitle('Move the XOR into a helper module')).toBe('Move the XOR into a…')
  expect(fallbackTitle('one two three four five')).toBe('one two three four five')
})

it('keeps the task and the latest turns of a long referenced chat', () => {
  const turns = Array.from({ length: 60 }, (_, index) => ({ role: index % 2 === 0 ? 'user' : 'assistant', text: `turn ${index} ${'x'.repeat(index === 59 ? 9000 : 3000)}` }))
  const snapshot = referenceSnapshot('s1', 'Long chat', turns) as { truncated?: boolean; turns: { text: string }[] }
  expect(snapshot.truncated).toBe(true)
  expect(JSON.stringify(snapshot).length).toBeLessThanOrEqual(64 * 1024)
  expect(snapshot.turns[0]!.text.startsWith('turn 0 ')).toBe(true)
  expect(snapshot.turns.at(-1)!.text.startsWith('turn 59 ')).toBe(true)
  expect(snapshot.turns.length).toBeGreaterThan(10)
  expect(referenceSnapshot('s2', 'Short', [{ role: 'user', text: 'hi' }])).toEqual({ sessionId: 's2', title: 'Short', turns: [{ role: 'user', text: 'hi' }] })
})

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
    expect(JSON.stringify(server.requests[0])).toContain('You are Rainy')
    const file = await readFile(join(h.home, 'chats', `${chat.id}.jsonl`), 'utf8')
    expect(file.split('\n').filter(Boolean).length).toBeGreaterThanOrEqual(6)
    expect(h.events.some(item => item.event === 'session.entry')).toBe(true)
  })

  it('keeps the title the user gave an untitled, already opened chat when its first message runs', async () => {
    server = new FakeOpenAI([{ text: 'Hi.' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    // Opening the chat loads its session with the untitled summary read from disk.
    await h.call('sessions.get', { sessionId: chat.id })
    await h.call('sessions.rename', { sessionId: chat.id, title: 'Heap notes' })
    await h.call('chat.send', { sessionId: chat.id, text: 'first message please' })
    await idle(h, chat.id)
    expect((await h.call('sessions.get', { sessionId: chat.id })).summary.title).toBe('Heap notes')
    expect((await h.call('sessions.list', {})).find(item => item.id === chat.id)?.title).toBe('Heap notes')
    const file = await readFile(join(h.home, 'chats', `${chat.id}.jsonl`), 'utf8')
    expect(file).not.toContain('"title":"first message please"')
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

  it('reads and edits a GBK file without touching its other Chinese text', async () => {
    server = new FakeOpenAI([
      { toolCalls: [{ name: 'read', arguments: { file_path: 'main.c' } }] },
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'main.c', old_string: 'int speed = 9600;', new_string: 'int speed = 115200; // 波特率' } }] },
      { text: 'Edited.' },
    ])
    await server.start()
    const h = await harness(server, cleanups)
    const gbk = (text: string): Buffer => Buffer.from(new Uint8Array([...text].flatMap((char) => {
      // GBK codes of the few Chinese characters this test uses.
      const codes: Record<string, number[]> = { 串: [0xb4, 0xae], 口: [0xbf, 0xda], 配: [0xc5, 0xe4], 置: [0xd6, 0xc3], 波: [0xb2, 0xa8], 特: [0xcc, 0xd8], 率: [0xc2, 0xca] }
      return codes[char] ?? [char.charCodeAt(0)]
    })))
    await writeFile(join(h.project, 'main.c'), gbk('/* 串口配置 */\r\nint speed = 9600;\r\n'))
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'raise the baud rate' })
    await idle(h, chat.id)
    const results = (await h.call('sessions.get', { sessionId: chat.id })).entries
      .filter((entry): entry is Extract<TranscriptEntry, { kind: 'toolResult' }> => entry.kind === 'toolResult')
    expect(JSON.stringify(results[0]!.content)).toContain('<encoding>GB18030 (GBK)</encoding>')
    expect(JSON.stringify(results[0]!.content)).toContain('1: /* 串口配置 */')
    expect(results[1]!.isError).toBe(false)
    expect((await readFile(join(h.project, 'main.c'))).equals(gbk('/* 串口配置 */\r\nint speed = 115200; // 波特率\r\n'))).toBe(true)
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

  it('shows a changed model window at once and follows the default after its own model is removed', async () => {
    server = new FakeOpenAI([{ text: 'first' }, { text: 'second' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'hello' })
    await idle(h, chat.id)
    expect((await h.call('sessions.get', { sessionId: chat.id })).context?.contextWindow).toBe(32768)
    const fake = { provider: 'fake', baseURL: server.baseURL, model: 'fake-model', local: true, api: 'openai-completions' as const }
    await h.call('models.configure', { ...fake, contextWindow: 65536 })
    const resized = await h.call('sessions.get', { sessionId: chat.id })
    expect(resized.context).toMatchObject({ contextWindow: 65536, inputLimit: resolveBudget(65536).inputLimit })
    await h.call('models.configure', { ...fake, provider: 'other', model: 'other-model', contextWindow: 32768 })
    await h.call('chat.setModel', { sessionId: chat.id, provider: 'other', model: 'other-model' })
    await h.call('models.select', { provider: 'fake', model: 'fake-model' })
    await h.call('models.remove', { provider: 'other' })
    expect((await h.call('sessions.get', { sessionId: chat.id })).model).toMatchObject({ provider: 'fake', model: 'fake-model' })
    await h.call('chat.send', { sessionId: chat.id, text: 'again' })
    await idle(h, chat.id)
    expect(server.requests.at(-1)).toMatchObject({ model: 'fake-model' })
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
    expect(result.message).toMatch(/^已压缩 \d+ 条历史（约 \d+ tokens）。$/)
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

  it('imports keys from the version 1 credentials file that released 1.x builds write', async () => {
    const { env, cleanup } = await tempHost()
    cleanups.push(cleanup)
    await mkdir(join(env.home, 'profiles', 'rainy'), { recursive: true })
    await writeFile(join(env.home, '.credentials.yaml'), [
      'version: 1', 'refs:', '  DEEPSEEK_API_KEY: sk-deepseek-123', '  RELAY_TOKEN: sk-relay-456', 'records:',
      '  client-connection/browser-session:', '    kind: token', '    payload:', '      version: 1', '      secret: not-a-model-key',
    ].join('\n'))
    await writeFile(join(env.home, 'profiles', 'rainy', 'cordis.patch.yml'), [
      '- id: llm-pi-ai', '  config:', '    providers:',
      '      rainy-deepseek:', '        baseURL: https://api.deepseek.com', '        api: openai-responses', '        apiKeyEnv: DEEPSEEK_API_KEY',
      '        models:', '          - id: deepseek-flash', '            contextWindow: 1000000',
      '      relay:', '        baseURL: https://relay.example', '        api: anthropic-messages', '        apiKeyEnv: RELAY_TOKEN',
      '        models:', '          - id: claude-opus-5-5', '            contextWindow: 200000',
    ].join('\n'))
    const settings = new Settings(env.home, {})
    await settings.load()
    expect(settings.apiKey('rainy-deepseek')).toBe('sk-deepseek-123')
    expect(settings.apiKey('relay')).toBe('sk-relay-456')
    expect(settings.providersWithKeys().sort()).toEqual(['rainy-deepseek', 'relay'])
    expect(JSON.stringify(settings.get())).not.toContain('sk-')
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
