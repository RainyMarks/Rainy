import { describe, it, expect, vi } from 'vitest'
import {
  CalibratedCounter,
  EndpointCounter,
  estimateRequest,
  estimateText,
  promptBreakdown,
  RequestQueue,
  resolveBudget,
} from '../../src/shared/budget.ts'

describe('request budget', () => {
  it('separates project memory and instructions without retaining their content', () => {
    const count = promptBreakdown(
      {
        messages: [
          {
            id: 'memory-recall',
            role: 'user',
            source: { kind: 'session-reference' },
            content: [{ type: 'text', text: 'Historical project fact' }],
          },
          {
            role: 'user',
            source: { kind: 'agent-instructions' },
            content: [{ type: 'text', text: 'Project constraint' }],
          },
        ],
      },
      [],
      ['memory-recall'],
    )
    expect(count.memory).toBeGreaterThan(0)
    expect(count.instructions).toBeGreaterThan(0)
    expect(count.history).toBe(0)
    expect(JSON.stringify(count)).not.toContain('Historical')
  })
  it('attributes selected extension descriptions once without double-counting system text', () => {
    const system = 'Core instructions.\n\nSelected skill details.'
    const count = promptBreakdown({ system, messages: [] }, ['Selected skill details.', 'absent server details'])
    expect(count.extensions).toBe(estimateText('Selected skill details.'))
    expect(count.system + count.extensions).toBe(estimateText(system))
    const inHistory = promptBreakdown({ messages: [{ role: 'system', content: [{ type: 'text', text: system }] }] }, [
      'Selected skill details.',
    ])
    expect(inHistory.extensions).toBe(count.extensions)
  })
  it('moves user-authored system instructions from system text to instructions alongside project instructions', () => {
    const system = 'Core instructions.\n\nSelected skill details.\n\nAlways answer in Chinese.'
    const project = { role: 'user', source: { kind: 'agent-instructions' }, content: [{ type: 'text', text: 'Project constraint' }] }
    const base = promptBreakdown({ system, messages: [project] }, ['Selected skill details.'])
    const count = promptBreakdown({ system, messages: [project] }, ['Selected skill details.'], [], ['Always answer in Chinese.', 'absent'])
    expect(count.instructions - base.instructions).toBe(estimateText('Always answer in Chinese.'))
    expect(count.system + count.extensions + count.instructions).toBe(base.system + base.extensions + base.instructions)
    expect(count.extensions).toBe(base.extensions)
  })
  it('requires the exact counter to identify its chat template', async () => {
    const fetch = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValueOnce(new Response('{"tokens":42,"model":"local-model"}'))
      .mockResolvedValueOnce(
        new Response('{"tokens":42,"model":"local-model","chatTemplate":"deployment-template-v1"}'),
      )
    try {
      const counter = new EndpointCounter('http://tokenizer.test/count', new CalibratedCounter())
      expect((await counter.count({ model: 'local-model', messages: [] })).kind).toBe('estimated')
      expect((await counter.count({ model: 'local-model', messages: [] })).kind).toBe('exact')
    } finally {
      fetch.mockRestore()
    }
  })
  it('requires an exact counter to acknowledge the deployed model', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{"tokens":42,"model":"wrong-model"}'))
    try {
      const counter = new EndpointCounter('http://tokenizer.test/count', new CalibratedCounter())
      expect((await counter.count({ model: 'local-model', messages: [] })).kind).toBe('estimated')
      const body = fetch.mock.calls[0]?.[1]?.body
      if (typeof body !== 'string') throw new Error('Expected a JSON tokenizer request body.')
      const parsed: unknown = JSON.parse(body)
      expect(parsed).toMatchObject({ model: 'local-model' })
    } finally {
      fetch.mockRestore()
    }
  })
  it('reserves input, output and error margin for 100K', () => {
    expect(resolveBudget(100000)).toEqual({
      contextWindow: 100000,
      outputTokens: 16000,
      marginTokens: 8000,
      inputLimit: 76000,
      compactAt: 70000,
      keepRecentTokens: 20000,
      summaryTokens: 4000,
      toolTokens: 4000,
    })
  })
  it.each([32768, 65536, 100000, 1000000])('fits the deployed %i window', (window) => {
    const budget = resolveBudget(window)
    expect(budget.inputLimit + budget.outputTokens + budget.marginTokens).toBe(window)
    expect(budget.keepRecentTokens).toBeLessThan(budget.compactAt)
    expect(budget.compactAt).toBeLessThanOrEqual(budget.inputLimit)
  })
  it('preserves official DeepSeek maximum output instead of applying local caps', () => {
    expect(resolveBudget(1048576, 393216).outputTokens).toBe(393216)
  })
  it('rejects an impossible output reservation', () => {
    expect(() => resolveBudget(32768, 32000)).toThrow()
  })
  it('prices multilingual text, framing and full tool schemas', () => {
    expect(estimateText('中文')).toBe(6)
    const request = { messages: [{ role: 'user', content: [{ type: 'text', text: '你好 hello' }] }] }
    expect(estimateRequest({ ...request, tools: [{ name: 'read', description: 'x'.repeat(1000) }] })).toBeGreaterThan(
      estimateRequest(request) + 300,
    )
  })
  it('prices a surrogate pair as one four-byte character and a lone surrogate as three bytes', () => {
    expect(estimateText('😀')).toBe(4)
    expect(estimateText('\ud83d')).toBe(3)
    expect(estimateText('\ude00\ud83d')).toBe(6)
    expect(estimateText('é\u0800abc')).toBe(6)
  })
  it('prices the complete serialized request, including content changed after an earlier estimate', () => {
    const serialized = (request: { system?: string; messages: { role: string; content: readonly unknown[] }[]; tools?: unknown[] }) =>
      estimateText(JSON.stringify({
        system: request.system,
        messages: request.messages.map(message => ({ role: message.role, content: message.content })),
        tools: request.tools,
      })) + 16 * (request.messages.length + 1)
    const block = Object.freeze({ type: 'text', text: '已完成 😀 \ud800 "done"' })
    const frozen = Object.freeze({ role: 'assistant', content: Object.freeze([block]) })
    const mutable = { role: 'user', content: [{ type: 'text', text: 'hello\nworld' }] }
    const request = { system: 'System 中文', messages: [frozen, mutable], tools: [{ name: 'read', description: 'Read.' }] }
    expect(estimateRequest(request)).toBe(serialized(request))
    mutable.content.push({ type: 'text', text: '新的输入 '.repeat(50) })
    expect(estimateRequest(request)).toBe(serialized(request))
    expect(estimateRequest({ messages: [] })).toBe(serialized({ messages: [] }))
    expect(promptBreakdown(request).history).toBe(
      estimateText(JSON.stringify(frozen.content)) + estimateText(JSON.stringify(mutable.content)),
    )
  })
  it('increases estimates after observed undercount', async () => {
    const counter = new CalibratedCounter()
    const request = { messages: [] }
    const before = await counter.count(request)
    counter.observe(before.tokens, before.tokens * 2)
    expect((await counter.count(request)).tokens).toBeGreaterThan(before.tokens * 2)
  })
  it('marks unavailable exact counters as estimates', async () => {
    const counter = new EndpointCounter('http://127.0.0.1:1', new CalibratedCounter())
    expect((await counter.count({ messages: [] })).kind).toBe('estimated')
  })
  it('passes only countable fields to the configured tokenizer', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('{"tokens":42}', { status: 200 }))
    try {
      const request = { messages: [], tools: [], transportOnly: 'must-not-leave' }
      const counter = new EndpointCounter('http://tokenizer.test/count', new CalibratedCounter())
      expect(await counter.count(request)).toEqual({
        tokens: 42,
        kind: 'exact',
        method: 'configured-request-tokenizer',
      })
      const body = fetch.mock.calls[0]?.[1]?.body
      if (typeof body !== 'string') throw new Error('Expected a JSON tokenizer request body.')
      const parsed: unknown = JSON.parse(body)
      expect(parsed).toEqual({ messages: [], tools: [] })
    } finally {
      fetch.mockRestore()
    }
  })
})

describe('endpoint request admission', () => {
  const settle = () => new Promise(resolve => setTimeout(resolve, 0))
  const track = (queue: RequestQueue, admitted: string[], name: string, key: string, limit: number, signal?: AbortSignal) =>
    queue.acquire(key, limit, signal).then((release) => {
      admitted.push(name)
      return release
    })

  it('serializes one endpoint while other endpoints run independently', async () => {
    const queue = new RequestQueue()
    const release = await queue.acquire('local', 1)
    let admitted = false
    const next = queue.acquire('local', 1).then((done) => {
      admitted = true
      done()
    })
    ;(await queue.acquire('other', 1))()
    await Promise.resolve()
    expect(admitted).toBe(false)
    release()
    await next
    expect(admitted).toBe(true)
  })
  it('cancelled waiters never release the active request', async () => {
    const queue = new RequestQueue()
    const active = await queue.acquire('local', 1)
    const abort = new AbortController()
    const cancelled = queue.acquire('local', 1, abort.signal)
    abort.abort(new Error('stop'))
    await expect(cancelled).rejects.toThrow('stop')
    let admitted = false
    const next = queue.acquire('local', 1).then((done) => {
      admitted = true
      done()
    })
    await Promise.resolve()
    expect(admitted).toBe(false)
    active()
    await next
    expect(admitted).toBe(true)
  })
  it('admits up to the key limit in arrival order', async () => {
    const queue = new RequestQueue()
    const admitted: string[] = []
    const first = await track(queue, admitted, 'first', 'remote', 2)
    const second = await track(queue, admitted, 'second', 'remote', 2)
    const [a, b, c] = ['a', 'b', 'c'].map(name => track(queue, admitted, name, 'remote', 2))
    ;(await queue.acquire('other', 2))()
    await settle()
    expect(admitted).toEqual(['first', 'second'])
    second()
    await settle()
    expect(admitted).toEqual(['first', 'second', 'a'])
    first()
    await settle()
    expect(admitted).toEqual(['first', 'second', 'a', 'b'])
    ;(await a)()
    await settle()
    expect(admitted).toEqual(['first', 'second', 'a', 'b', 'c'])
    ;(await b)()
    ;(await c)()
  })
  it('ignores a repeated release', async () => {
    const queue = new RequestQueue()
    const admitted: string[] = []
    const active = await queue.acquire('local', 1)
    const [a, b] = ['a', 'b'].map(name => track(queue, admitted, name, 'local', 1))
    active()
    active()
    await settle()
    expect(admitted).toEqual(['a'])
    ;(await a)()
    ;(await b)()
    expect(admitted).toEqual(['a', 'b'])
  })
  it('a cancelled waiter neither holds nor frees a slot and stops blocking later waiters', async () => {
    const queue = new RequestQueue()
    const admitted: string[] = []
    const active = await queue.acquire('shared', 1)
    const abort = new AbortController()
    const head = track(queue, admitted, 'head', 'shared', 1, abort.signal)
    const later = track(queue, admitted, 'later', 'shared', 2)
    const last = track(queue, admitted, 'last', 'shared', 2)
    await settle()
    expect(admitted).toEqual([])
    abort.abort(new Error('stop'))
    await expect(head).rejects.toThrow('stop')
    await settle()
    expect(admitted).toEqual(['later'])
    ;(await later)()
    ;(await last)()
    active()
    expect(admitted).toEqual(['later', 'last'])
  })
  it('returns the slot of a waiter cancelled as it is admitted', async () => {
    const queue = new RequestQueue()
    const admitted: string[] = []
    const active = await queue.acquire('local', 1)
    const abort = new AbortController()
    const cancelled = track(queue, admitted, 'cancelled', 'local', 1, abort.signal)
    const next = track(queue, admitted, 'next', 'local', 1)
    active()
    abort.abort(new Error('stop'))
    await expect(cancelled).rejects.toThrow('stop')
    ;(await next)()
    expect(admitted).toEqual(['next'])
  })
})
