import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { HostEvents, RetryWait, TranscriptEntry } from '../../src/shared/rpc.ts'
import { AUTO_RETRIES, isPassingFailure, RETRY_BASE_MS, retryDelay } from '../../src/host/agent/retry.ts'
import { FakeOpenAI, harness, idle, type Harness } from './agent-fixtures.ts'

type Assistant = Extract<TranscriptEntry, { kind: 'assistant' }>

function failed(errorMessage: string): AssistantMessage {
  return {
    role: 'assistant', content: [], api: 'openai-completions', provider: 'fake', model: 'fake-model', stopReason: 'error', errorMessage, timestamp: 0,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  }
}

describe('retry policy', () => {
  it('retries overload, rate limits, dropped connections and timeouts, but not refused or quota-limited requests', () => {
    expect(isPassingFailure(failed('503 Service Unavailable'))).toBe(true)
    expect(isPassingFailure(failed('429 Too Many Requests'))).toBe(true)
    expect(isPassingFailure(failed('terminated'))).toBe(true)
    expect(isPassingFailure(failed('模型在 300 秒内没有返回数据，请求已超时。'))).toBe(true)
    expect(isPassingFailure(failed('服务器繁忙，请稍后重试'))).toBe(true)
    expect(isPassingFailure(failed('401 bad key'))).toBe(false)
    expect(isPassingFailure(failed('400 max_tokens must be at most 5000'))).toBe(false)
    expect(isPassingFailure(failed('429 insufficient_quota: billing'))).toBe(false)
    expect(isPassingFailure({ ...failed('503'), stopReason: 'aborted' })).toBe(false)
  })

  it('backs off exponentially with jitter and follows a delay the provider asks for', () => {
    expect(retryDelay(1, 'x', () => 0.5)).toBe(RETRY_BASE_MS)
    expect(retryDelay(2, 'x', () => 0.5)).toBe(RETRY_BASE_MS * 2)
    expect(retryDelay(3, 'x', () => 0.5)).toBe(RETRY_BASE_MS * 4)
    expect(retryDelay(1, 'x', () => 0)).toBe(Math.round(RETRY_BASE_MS * 0.85))
    expect(retryDelay(1, 'x', () => 0.999_999)).toBeLessThanOrEqual(Math.round(RETRY_BASE_MS * 1.15))
    expect(retryDelay(1, 'Server requested 30s retry delay (max: 10s). Busy')).toBe(30_000)
    expect(retryDelay(1, 'Server requested 1s retry delay (max: 0s).')).toBe(RETRY_BASE_MS)
    expect(retryDelay(1, 'Server requested 600s retry delay (max: 10s).')).toBeUndefined()
  })
})

describe('agent retries', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI
  beforeEach(() => { cleanups.length = 0 })
  afterEach(async () => {
    for (const cleanup of cleanups.reverse()) await cleanup()
    await server?.stop()
  })

  const states = (h: Harness): HostEvents['session.state'][] => h.events.filter(item => item.event === 'session.state').map(item => item.data as HostEvents['session.state'])

  /** Wait for the chat's next automatic-retry countdown after `seen` earlier ones. */
  async function nextWait(h: Harness, seen: number): Promise<RetryWait> {
    for (let attempt = 0; attempt < 400; attempt++) {
      const waits = states(h).flatMap(state => state.retry === undefined ? [] : [state.retry]).filter((wait, index, all) => all.findIndex(other => other.at === wait.at) === index)
      if (waits.length > seen) return waits[seen]!
      await new Promise(resolve => setTimeout(resolve, 10))
    }
    throw new Error('no retry wait')
  }

  it('waits before retrying a dropped request, retries at once on request and keeps the failure in the transcript', async () => {
    server = new FakeOpenAI([{ drop: 'half an ans' }, { text: 'recovered' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    const before = Date.now()
    await h.call('chat.send', { sessionId: chat.id, text: 'hi' })
    const wait = await nextWait(h, 0)
    expect(wait).toMatchObject({ attempt: 1, max: AUTO_RETRIES })
    expect(wait.at - before).toBeGreaterThanOrEqual(RETRY_BASE_MS * 0.85 - 50)
    expect((await h.call('sessions.get', { sessionId: chat.id })).retry).toEqual(wait)
    expect(server.requests).toHaveLength(1)

    await h.call('chat.retry', { sessionId: chat.id })
    await idle(h, chat.id)
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    expect(snapshot.retry).toBeUndefined()
    expect(snapshot.summary.status).toBe('idle')
    expect(snapshot.entries.map(entry => entry.kind === 'notice' ? `notice:${entry.code}` : entry.kind)).toEqual(['user', 'assistant', 'notice:auto-retry', 'assistant', 'turn'])
    const [first, second] = snapshot.entries.filter((entry): entry is Assistant => entry.kind === 'assistant')
    expect(first!.message.stopReason).toBe('error')
    expect(JSON.stringify(second!.message.content)).toContain('recovered')
    // The dropped half answer is not replayed.
    expect(JSON.stringify(server.requests[1])).not.toContain('half an ans')
    expect(states(h).at(-1)?.retry).toBeUndefined()
  })

  it('gives up after the automatic retries and then retries when the user asks', async () => {
    server = new FakeOpenAI([{ drop: 'a' }, { drop: 'b' }, { drop: 'c' }, { drop: 'd' }, { text: 'finally' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'hi' })
    for (let attempt = 0; attempt < AUTO_RETRIES; attempt++) {
      expect((await nextWait(h, attempt)).attempt).toBe(attempt + 1)
      await h.call('chat.retry', { sessionId: chat.id })
    }
    await idle(h, chat.id)
    let snapshot = await h.call('sessions.get', { sessionId: chat.id })
    expect(server.requests).toHaveLength(AUTO_RETRIES + 1)
    expect(snapshot.retry).toBeUndefined()
    expect((snapshot.entries.findLast(entry => entry.kind === 'assistant') as Assistant).message.stopReason).toBe('error')

    await h.call('chat.retry', { sessionId: chat.id })
    await idle(h, chat.id)
    snapshot = await h.call('sessions.get', { sessionId: chat.id })
    expect(JSON.stringify((snapshot.entries.findLast(entry => entry.kind === 'assistant') as Assistant).message.content)).toContain('finally')
    expect(snapshot.entries.filter(entry => entry.kind === 'user')).toHaveLength(1)
    await expect(h.call('chat.retry', { sessionId: chat.id })).rejects.toThrow(/invalid/)
  })

  it('stops waiting when the user stops the run', async () => {
    server = new FakeOpenAI([{ drop: 'x' }, { text: 'never asked' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'hi' })
    await nextWait(h, 0)
    await h.call('chat.abort', { sessionId: chat.id })
    await idle(h, chat.id)
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    expect(server.requests).toHaveLength(1)
    expect(snapshot.retry).toBeUndefined()
    expect(snapshot.entries.at(-1)).toMatchObject({ kind: 'notice', code: 'stopped' })
  })

  it('does not wait to retry a request the provider refused', async () => {
    server = new FakeOpenAI([{ status: 401, body: '{"error":{"message":"bad key"}}' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'hi' })
    await idle(h, chat.id)
    expect(states(h).some(state => state.retry !== undefined)).toBe(false)
    expect(server.requests).toHaveLength(1)
  })

  it('records how long each thinking block took', async () => {
    server = new FakeOpenAI([{ thinking: 'Let me weigh the options first.', pauseMs: 80, text: 'Answer.' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'think' })
    await idle(h, chat.id)
    const reply = (await h.call('sessions.get', { sessionId: chat.id })).entries.find((entry): entry is Assistant => entry.kind === 'assistant')!
    const index = reply.message.content.findIndex(block => block.type === 'thinking')
    expect(index).toBeGreaterThanOrEqual(0)
    expect(reply.thinkingMs?.[index]).toBeGreaterThanOrEqual(60)
    expect(reply.thinkingMs?.[index]).toBeLessThan(5_000)
  })
})
