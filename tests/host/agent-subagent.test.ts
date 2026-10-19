/** The `task` sub-agent: its own transcript and read-only tools, and only its report reaching the chat. */
import { mkdir, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, beforeEach, expect, it } from 'vitest'
import type { TranscriptEntry } from '../../src/shared/rpc.ts'
import { runSubagent, subagentPrompt } from '../../src/host/agent/subagent.ts'
import { inertCoreTools } from '../../src/host/agent/tools/index.ts'
import { FakeOpenAI, harness, idle, type Harness } from './agent-fixtures.ts'

const cleanups: (() => Promise<void>)[] = []
let server: FakeOpenAI
beforeEach(() => { cleanups.length = 0 })
afterEach(async () => {
  for (const cleanup of cleanups.reverse()) await cleanup()
  await server?.stop()
})

const text = (entry: Extract<TranscriptEntry, { kind: 'toolResult' }>): string =>
  entry.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')

it('runs a sub-agent that reads files on its own and gives the chat only its report', async () => {
  // The chat calls task; the sub-agent greps, reads, then reports; the chat answers.
  server = new FakeOpenAI([
    { toolCalls: [{ name: 'task', arguments: { description: 'Find the marker', prompt: 'Find which file contains rainy-marker and report its path.' } }] },
    { toolCalls: [{ name: 'grep', arguments: { pattern: 'rainy-marker' } }] },
    { toolCalls: [{ name: 'read', arguments: { file_path: 'notes/where.txt' } }] },
    { text: 'The marker is in notes/where.txt on line 1.' },
    { text: 'It lives in notes/where.txt.' },
  ])
  await server.start()
  const h: Harness = await harness(server, cleanups)
  await mkdir(join(h.project, 'notes'), { recursive: true })
  await writeFile(join(h.project, 'notes', 'where.txt'), 'rainy-marker here\n')
  const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
  await h.call('chat.send', { sessionId: chat.id, text: 'where is the marker?' })
  await idle(h, chat.id)

  const snapshot = await h.call('sessions.get', { sessionId: chat.id })
  const results = snapshot.entries.filter((entry): entry is Extract<TranscriptEntry, { kind: 'toolResult' }> => entry.kind === 'toolResult')
  expect(results).toHaveLength(1)
  expect(results[0]!.toolName).toBe('task')
  expect(text(results[0]!)).toContain('notes/where.txt')
  expect(results[0]!.details).toMatchObject({ description: 'Find the marker', activity: ['grep', 'read'] })

  // The sub-agent's own tool calls never entered the chat's transcript.
  expect(snapshot.entries.some(entry => entry.kind === 'toolResult' && (entry.toolName === 'grep' || entry.toolName === 'read'))).toBe(false)
  // It was asked with its own prompt and its own tools, which cannot change anything.
  const subRequest = server.requests[1] as { messages: { role: string; content: string }[]; tools: { function: { name: string } }[] }
  expect(subRequest.messages[0]!.content).toContain('You are a sub-agent of Rainy')
  expect(subRequest.tools.map(tool => tool.function.name).sort()).toEqual(['glob', 'grep', 'read'])
})

it('reports a sub-agent that answers nothing, without failing the chat', async () => {
  server = new FakeOpenAI([
    { toolCalls: [{ name: 'task', arguments: { description: 'Look around', prompt: 'Look for something that is not there.' } }] },
    { text: '' },
    { text: 'Nothing came back.' },
  ])
  await server.start()
  const h = await harness(server, cleanups)
  const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
  await h.call('chat.send', { sessionId: chat.id, text: 'look around' })
  await idle(h, chat.id)
  const snapshot = await h.call('sessions.get', { sessionId: chat.id })
  const result = snapshot.entries.find((entry): entry is Extract<TranscriptEntry, { kind: 'toolResult' }> => entry.kind === 'toolResult' && entry.toolName === 'task')
  expect(result?.isError).not.toBe(true)
  expect(text(result!)).toContain('reported nothing')
})

it('runs the sub-agents of one step at the same time', async () => {
  // Two tasks in one step. Requests to a loopback model server are queued one at a time by the Host, so the
  // overlap shows in the chat's running tools, not in the clock: both task calls are under way before either ends.
  server = new FakeOpenAI([
    { toolCalls: [
      { name: 'task', arguments: { description: 'First look', prompt: 'Report A.' } },
      { name: 'task', arguments: { description: 'Second look', prompt: 'Report B.' } },
    ] },
    { text: 'Report of one sub-agent.', pauseMs: 300 },
    { text: 'Report of the other sub-agent.', pauseMs: 300 },
    { text: 'Both reported.' },
  ])
  await server.start()
  const h = await harness(server, cleanups)
  const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
  await h.call('chat.send', { sessionId: chat.id, text: 'look twice' })
  let mostAtOnce = 0
  for (let attempt = 0; attempt < 400; attempt++) {
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    mostAtOnce = Math.max(mostAtOnce, snapshot.runningTools.length)
    if (snapshot.summary.status !== 'running') break
    await new Promise(resolve => setTimeout(resolve, 10))
  }
  const snapshot = await h.call('sessions.get', { sessionId: chat.id })
  const results = snapshot.entries.filter((entry): entry is Extract<TranscriptEntry, { kind: 'toolResult' }> => entry.kind === 'toolResult' && entry.toolName === 'task')
  expect(results).toHaveLength(2)
  expect(results.map(result => text(result)).sort()).toEqual(['Report of one sub-agent.', 'Report of the other sub-agent.'])
  expect(mostAtOnce).toBe(2)
})

it('ends a model request in flight when the time limit passes', async () => {
  // The fake stream never answers by itself; it only ends when its signal aborts.
  const signals: AbortSignal[] = []
  const streamFn = ((_model: unknown, _context: unknown, options: { signal?: AbortSignal }) => {
    const signal = options.signal!
    signals.push(signal)
    const aborted = new Promise<void>(done => { signal.addEventListener('abort', () => { done() }, { once: true }) })
    return {
      async *[Symbol.asyncIterator]() { await aborted },
      result: async () => {
        await aborted
        return {
          role: 'assistant', content: [], api: 'openai-completions', provider: 'test', model: 'm', stopReason: 'aborted', timestamp: 0,
          usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
        }
      },
    }
  }) as never
  const started = Date.now()
  const report = await runSubagent({
    prompt: 'Take forever.', description: 'Slow', cwd: '/project', platform: 'linux', tools: [], streamFn, onProgress: () => undefined, maxMs: 300,
    resolved: { setup: {}, model: { id: 'm', provider: 'test', api: 'openai-completions', input: ['text'], contextWindow: 1000, maxTokens: 100 }, thinking: undefined } as never,
  })
  expect(Date.now() - started).toBeLessThan(2000)
  expect(signals).toHaveLength(1)
  expect(signals[0]!.aborted).toBe(true)
  expect(report.stopped).toBe('time')
})

it('tells the sub-agent where it works and that it cannot change anything', () => {
  const prompt = subagentPrompt('C:\\work\\app', 'win32')
  expect(prompt).toContain('C:\\work\\app')
  expect(prompt).toContain('Windows')
  expect(prompt).toContain('cannot change files, run commands or ask questions')
})

it('declares task in an estimate, so the context meter counts what a chat sends', () => {
  expect(inertCoreTools('linux').map(tool => tool.name)).toContain('task')
})
