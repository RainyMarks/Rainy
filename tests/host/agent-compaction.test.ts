/** Compaction chooses an older span that leaves the latest user request verbatim and never separates a tool call from its result. */
import { expect, it } from 'vitest'
import type { AssistantMessage, Message } from '@earendil-works/pi-ai'
import { resolveBudget } from '../../src/shared/budget.ts'
import { compactionThreshold, selectSpan } from '../../src/host/agent/compaction.ts'
import type { ContextItem } from '../../src/host/agent/context.ts'

const budget = resolveBudget(32768, 4096)
const filler = 'x'.repeat(4000)

function assistant(content: AssistantMessage['content']): Message {
  return {
    role: 'assistant', content, api: 'openai-completions', provider: 'fake', model: 'm', stopReason: content.some(block => block.type === 'toolCall') ? 'toolUse' : 'stop', timestamp: 1,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  }
}

/** One turn: a user request, a tool call, its result and an answer, each with filler so the turn has weight. */
function turn(index: number): ContextItem[] {
  return [
    { entryId: `u${index}`, message: { role: 'user', content: `request ${index} ${filler}`, timestamp: 1 } },
    { entryId: `a${index}`, message: assistant([{ type: 'toolCall', id: `c${index}`, name: 'read', arguments: { file_path: `f${index}` } }]) },
    { entryId: `r${index}`, message: { role: 'toolResult', toolCallId: `c${index}`, toolName: 'read', content: [{ type: 'text', text: filler }], isError: false, timestamp: 1 } },
    { entryId: `b${index}`, message: assistant([{ type: 'text', text: `answer ${index} ${filler}` }]) },
  ]
}

it('triggers below the input limit and keeps the latest user request out of the span', () => {
  expect(compactionThreshold(budget)).toBeLessThan(budget.inputLimit)
  const items = [...turn(1), ...turn(2), ...turn(3), ...turn(4)]
  const span = selectSpan(items, budget, 'u4', false)
  expect(span).toBeDefined()
  const [start, end] = span!
  expect(start).toBe(0)
  expect(end).toBeLessThan(items.findIndex(item => item.entryId === 'u4'))
})

it('never ends a span between a tool call and its result', () => {
  const items = [...turn(1), ...turn(2), ...turn(3)]
  for (const manual of [false, true]) {
    const span = selectSpan(items, budget, undefined, manual)
    if (span === undefined) continue
    const last = items[span[1]]!.message
    expect(last.role === 'assistant' && last.content.some(block => block.type === 'toolCall')).toBe(false)
  }
})

it('compacts the work after the latest request when everything before it is too small', () => {
  const items = [{ entryId: 'u1', message: { role: 'user' as const, content: 'one long task', timestamp: 1 } }, ...turn(2).slice(1), ...turn(3).slice(1), ...turn(4).slice(1)]
  const span = selectSpan(items, budget, 'u1', true)
  expect(span?.[0]).toBe(1)
})

it('leaves short chats alone', () => {
  expect(selectSpan(turn(1).map(item => ({ ...item, message: item.message.role === 'user' ? { ...item.message, content: 'hi' } : item.message })).slice(0, 1), budget, 'u1', true)).toBeUndefined()
})
