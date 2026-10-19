import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { HostEvents, TranscriptEntry } from '../../src/shared/rpc.ts'
import { applyStreamPatch, compactStreaming, streamBaseline, streamPatch } from '../../src/shared/stream-patch.ts'
import { FakeOpenAI, harness, type Harness } from './agent-fixtures.ts'

/** Wait for the chat to finish from its events alone: loading it would move the stream's baseline to the loaded copy. */
async function finished(h: Harness, sessionId: string): Promise<void> {
  for (let attempt = 0; attempt < 400; attempt++) {
    if (h.events.some(({ event, data }) => event === 'session.state' && (data as HostEvents['session.state']).sessionId === sessionId
      && (data as HostEvents['session.state']).status === 'idle') && h.events.some(({ event }) => event === 'session.entry')) {
      const last = h.events.findLast(({ event }) => event === 'session.state')?.data as HostEvents['session.state'] | undefined
      if (last?.status === 'idle') return
    }
    await new Promise(resolve => setTimeout(resolve, 25))
  }
  throw new Error('chat did not finish')
}

function message(content: AssistantMessage['content']): AssistantMessage {
  return {
    role: 'assistant', content, api: 'openai-completions', provider: 'fake', model: 'fake-model', stopReason: 'stop', timestamp: 1,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  }
}

describe('stream patches', () => {
  it('send what text and reasoning gained, and blocks that appeared or changed otherwise', () => {
    const first = message([{ type: 'thinking', thinking: 'Let me' }])
    const second = message([{ type: 'thinking', thinking: 'Let me think.' }, { type: 'text', text: 'Hel' }])
    const { patch, baseline } = streamPatch(streamBaseline(first), second)
    expect(patch).toEqual({ append: [{ index: 0, at: 6, text: ' think.' }], blocks: [{ index: 1, block: { type: 'text', text: 'Hel' } }] })
    expect(applyStreamPatch(first, patch!)).toEqual(second)

    const third = message([{ type: 'thinking', thinking: 'Let me think.', thinkingSignature: 'sig' }, { type: 'text', text: 'Hello' }])
    const next = streamPatch(baseline, third).patch!
    // A signature arriving changes the block otherwise, so it goes whole; the text only grew.
    expect(next).toEqual({ append: [{ index: 1, at: 3, text: 'lo' }], blocks: [{ index: 0, block: third.content[0] }] })
    expect(applyStreamPatch(second, next)).toEqual(third)
    expect(streamPatch(streamBaseline(third), third).patch).toEqual({})
  })

  it('fall back to the whole message when text was rewritten, blocks went away or the message changed', () => {
    const before = message([{ type: 'text', text: 'abcdefghijklmnopqrstuvwxyz' }])
    const rewritten = message([{ type: 'text', text: 'abcdefghijklmnopqrstuvwXYZ and more' }])
    expect(streamPatch(streamBaseline(before), rewritten).patch).toEqual({ blocks: [{ index: 0, block: rewritten.content[0] }] })
    expect(streamPatch(streamBaseline(before), message([])).patch).toBeUndefined()
    expect(streamPatch(streamBaseline(before), { ...before, stopReason: 'length' }).patch).toBeUndefined()
  })

  it('do not apply to a copy that does not match', () => {
    const page = message([{ type: 'text', text: 'Hello wor' }])
    expect(applyStreamPatch(page, { append: [{ index: 0, at: 5, text: ' world' }] })).toBeUndefined()
    expect(applyStreamPatch(null, { append: [{ index: 0, at: 0, text: 'x' }] })).toBeUndefined()
  })

  it('leave out the providers\' scratch fields and show the arguments named so far', () => {
    const streaming = message([{ type: 'toolCall', id: 'c1', name: 'write', arguments: {},
      partialArgs: '{"file_path": "src/a\\"b.ts", "content": "line 1\\nline 2', streamIndex: 0 } as never])
    expect(compactStreaming(streaming).content).toEqual([{ type: 'toolCall', id: 'c1', name: 'write', arguments: { file_path: 'src/a"b.ts' } }])
    const done = message([{ type: 'toolCall', id: 'c1', name: 'write', arguments: { file_path: 'a.ts', content: 'x' } }])
    expect(compactStreaming(done)).toEqual(done)
  })
})

describe('streaming to the page', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI
  beforeEach(() => { cleanups.length = 0 })
  afterEach(async () => {
    for (const cleanup of cleanups.reverse()) await cleanup()
    await server?.stop()
  })

  it('sends a long reply whole once and then only what it gained', async () => {
    const text = Array.from({ length: 60 }, (_, index) => `Paragraph ${index}.`).join(' ')
    server = new FakeOpenAI([{ text, chunkMs: 8 }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'write a lot' })
    await finished(h, chat.id)
    // The events hold what was sent; copy each as the WebSocket would have.
    const updates = h.events.filter(item => item.event === 'session.stream').map(item => item.data as HostEvents['session.stream'])
    const patches = updates.filter(update => 'patch' in update)
    expect(patches.length).toBeGreaterThan(3)
    // The page's copy, rebuilt from the events, ends as the reply did.
    let page: AssistantMessage | null = null
    let sent = 0
    let whole = 0
    for (const update of updates) {
      sent += JSON.stringify(update).length
      if ('patch' in update) {
        const next = applyStreamPatch(page, update.patch)
        expect(next).toBeDefined()
        page = next!
      } else if (update.message !== null) page = update.message
      // What sending the whole reply on every update would have cost.
      if (page !== null) whole += JSON.stringify({ sessionId: chat.id, message: page }).length
    }
    const reply = (await h.call('sessions.get', { sessionId: chat.id })).entries.find((entry): entry is Extract<TranscriptEntry, { kind: 'assistant' }> => entry.kind === 'assistant')!
    const streamed = page?.content.find(block => block.type === 'text')
    expect(reply.message.content).toContainEqual({ type: 'text', text })
    expect(text.startsWith(streamed?.type === 'text' ? streamed.text : '?')).toBe(true)
    expect(sent).toBeLessThan(whole / 3)
  })
})
