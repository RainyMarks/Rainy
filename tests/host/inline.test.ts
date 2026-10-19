/** Editor AI on the Host: suggestions stay off unless enabled, replies are cleaned to what fits, and rewrites stream and stop. */
import { describe, expect, it, vi } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import { cleanEdit, cleanSuggestion, InlineAssist } from '../../src/host/agent/inline.ts'
import { DEFAULT_PREFS } from '../../src/host/settings.ts'
import type { UiPreferences } from '../../src/shared/rpc.ts'

function reply(text: string, stopReason: AssistantMessage['stopReason'] = 'stop', errorMessage?: string): AssistantMessage {
  return {
    role: 'assistant', content: [{ type: 'text', text }], api: 'openai-completions', provider: 'test', model: 'm',
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
    stopReason, timestamp: 0, ...errorMessage === undefined ? {} : { errorMessage },
  }
}

function harness(prefs: Partial<UiPreferences> = {}, models: { provider: string; model: string }[] = [{ provider: 'fast', model: 'f1' }, { provider: 'main', model: 'm1' }]) {
  const resolved: unknown[] = []
  const complete = vi.fn(async (_resolved: unknown, _prompt: string, _tools: unknown[], _timeout: number, _options: { signal?: AbortSignal }) => reply('x + 1'))
  const streamed: string[] = []
  let finalText = 'return a * b'
  const streamFn = vi.fn((_model: unknown, _context: unknown, options: { signal: AbortSignal }) => {
    const events = ['return a', 'return a * b'].map(text => ({ type: 'text_delta', partial: reply(text) }))
    return {
      async *[Symbol.asyncIterator]() {
        for (const event of events) {
          await new Promise(resolve => setTimeout(resolve, 100))
          yield event
        }
      },
      result: async () => options.signal.aborted ? reply('', 'aborted') : reply(finalText),
    }
  })
  const fakeModels = {
    resolve: (selection?: { provider: string; model: string }) => { resolved.push(selection); return models.length === 0 ? undefined : { setup: {}, model: {}, thinking: undefined } },
    complete, streamFn,
  }
  const settings = { prefs: () => ({ ...DEFAULT_PREFS, ...prefs }), get: () => ({ models }) }
  const assist = new InlineAssist(fakeModels as never, settings as never, (_id, text) => { streamed.push(text) })
  return { assist, complete, streamFn, resolved, streamed, setFinal: (text: string) => { finalText = text } }
}

describe('cleaning replies', () => {
  it('keeps only what fits at the cursor', () => {
    expect(cleanSuggestion('```python\nreturn x\n```', { prefix: 'def f(x):\n    ', suffix: '\n' })).toBe('return x')
    // The model repeated the start of the line.
    expect(cleanSuggestion('    total = sum(xs)', { prefix: 'def f(xs):\n    total = ', suffix: '' })).toBe('sum(xs)')
    // In the middle of a line: one line, without what already follows the cursor.
    expect(cleanSuggestion('a, b)\nprint(1)', { prefix: 'print(', suffix: ')\n' })).toBe('a, b')
    expect(cleanSuggestion('  \n ', { prefix: 'x = ', suffix: '' })).toBe('')
    expect(cleanSuggestion(Array.from({ length: 30 }, (_value, index) => `line${index}`).join('\n'), { prefix: '', suffix: '' }).split('\n')).toHaveLength(16)
  })

  it('keeps the replacement of a selection to the region', () => {
    expect(cleanEdit('```js\nconst a = 1\n```', 'let a = 1')).toBe('const a = 1')
    expect(cleanEdit('\n\nconst a = 1\n\n', 'let a = 1')).toBe('const a = 1')
    expect(cleanEdit('<|selection_start|>x<|selection_end|>', 'y')).toBe('x')
    expect(cleanEdit('x\n', 'y\n')).toBe('x\n')
  })
})

describe('suggestions', () => {
  it('asks nothing while suggestions are off', async () => {
    const h = harness({ inlineCompletion: false })
    expect(await h.assist.complete({ path: 'a.py', prefix: 'x = ', suffix: '' })).toEqual({ text: '' })
    expect(h.streamFn).not.toHaveBeenCalled()
  })

  it('uses the chosen model, or the chat model when none is chosen or it was removed', async () => {
    const h = harness({ inlineCompletion: true, completionModel: 'fast' })
    h.setFinal('x + 1')
    expect(await h.assist.complete({ path: 'a.py', language: 'python', prefix: 'y = x', suffix: '' })).toEqual({ text: 'x + 1' })
    const context = h.streamFn.mock.calls[0]![1] as { messages: { role: string; content: string }[] }
    const prompt = context.messages.map(message => (typeof message.content === 'string' ? message.content : '')).join(' ')
    expect(prompt).toContain('File: a.py (python)')
    expect(prompt).toContain('y = x<|cursor|>')
    expect(prompt).toContain('code completion engine')
    expect(h.resolved).toEqual([{ provider: 'fast', model: 'f1' }])
    const fallback = harness({ inlineCompletion: true, completionModel: 'gone' })
    await fallback.assist.complete({ path: 'a', prefix: '', suffix: '' })
    expect(fallback.resolved).toEqual([undefined])
  })

  it('shows the suggestion as it streams, for a request that asked to follow it', async () => {
    const h = harness({ inlineCompletion: true })
    h.setFinal('return a * b')
    const request = { requestId: 'suggest-1', path: 'a.ts', prefix: 'function f(a, b) {\n  ', suffix: '\n}' }
    expect(await h.assist.complete(request)).toEqual({ text: 'return a * b' })
    expect(h.streamed).toEqual(['return a', 'return a * b'])
    // A suggestion is short; the request says so, instead of letting the model write on until the timeout.
    expect(h.streamFn.mock.calls[0]![2]).toMatchObject({ maxTokens: 192 })
    // Without a request id there is nobody to show progress to, so none is sent.
    h.streamed.length = 0
    await h.assist.complete({ path: 'a.ts', prefix: 'x', suffix: '' })
    expect(h.streamed).toEqual([])
  })

  it('ends the previous request and reports failures without throwing', async () => {
    const h = harness({ inlineCompletion: true })
    const first = h.assist.complete({ path: 'a', prefix: 'a', suffix: '' })
    const firstSignal = (h.streamFn.mock.calls[0]![2] as { signal: AbortSignal }).signal
    const failing = harness({ inlineCompletion: true })
    failing.streamFn.mockImplementationOnce(() => ({
      async *[Symbol.asyncIterator]() { /* nothing streams before the failure */ },
      result: async () => reply('', 'error', 'quota exceeded'),
    }))
    const second = await failing.assist.complete({ path: 'a', prefix: 'ab', suffix: '' })
    const third = await h.assist.complete({ path: 'a', prefix: 'ab', suffix: '' })
    expect(firstSignal.aborted).toBe(true)
    expect(await first).toEqual({ text: '' })
    expect(third).toEqual({ text: 'return a * b' })
    expect(second).toEqual({ text: '', error: 'quota exceeded' })
    expect(await harness({ inlineCompletion: true }, []).assist.complete({ path: 'a', prefix: '', suffix: '' })).toMatchObject({ text: '' })
  })
})

describe('rewrites', () => {
  it('streams progress and returns the cleaned replacement', async () => {
    const h = harness()
    h.setFinal('```\nreturn a * b\n```')
    const result = await h.assist.edit({ requestId: 'r1', path: 'm.py', before: 'def f(a, b):\n', selection: '    return a + b', after: '\n', instruction: 'multiply' })
    expect(result).toEqual({ text: 'return a * b' })
    expect(h.streamed.length).toBeGreaterThan(0)
    const context = h.streamFn.mock.calls[0]![1] as { messages: { role: string; content: unknown }[] }
    expect(JSON.stringify(context.messages)).toContain('<|selection_start|>    return a + b<|selection_end|>')
    expect(JSON.stringify(context.messages)).toContain('Instruction: multiply')
  })

  it('stops when cancelled and needs a model', async () => {
    const h = harness()
    const pending = h.assist.edit({ requestId: 'r2', path: 'm.py', before: '', selection: 'x', after: '', instruction: 'y' })
    h.assist.cancel('r2')
    await expect(pending).rejects.toThrow()
    await expect(harness({}, []).assist.edit({ requestId: 'r3', path: 'm.py', before: '', selection: '', after: '', instruction: 'y' })).rejects.toThrow()
  })
})
