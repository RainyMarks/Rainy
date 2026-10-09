/** Model context from transcript entries: compactions applied, UI-only entries dropped. */
import type { AssistantMessage, ImageContent, Message, TextContent, ToolResultMessage, UserMessage } from '@earendil-works/pi-ai'
import type { TranscriptEntry } from '../../shared/rpc.ts'

/**
 * The user message that stands in for a compacted span.
 * @param summary Checkpoint text written by the compaction request.
 * @returns The framed message text.
 */
export function frameSummary(summary: string): string {
  return 'This is an automatically generated checkpoint condensing an earlier span of the conversation to free up context. '
    + 'Treat the captured context as established background and build on it without restating it. '
    + 'Continue the task directly from the messages that follow, without acknowledging this checkpoint.\n\n'
    + `<compacted-summary>\n${summary}\n</compacted-summary>`
}

/** A model message and the transcript entry it came from. */
export interface ContextItem {
  entryId: string
  message: Message
}

/**
 * Convert one entry to its model message.
 * @param entry Transcript entry.
 * @returns The message, or `undefined` for entries the model never sees.
 */
export function entryMessage(entry: TranscriptEntry): Message | undefined {
  switch (entry.kind) {
    case 'user': {
      const content: (TextContent | ImageContent)[] = [{ type: 'text', text: entry.text }, ...(entry.images ?? [])]
      return { role: 'user', content: entry.images?.length ? content : entry.text, timestamp: entry.ts } satisfies UserMessage
    }
    case 'context': return { role: 'user', content: entry.text, timestamp: entry.ts } satisfies UserMessage
    // Failed and stopped replies are incomplete turns; replaying them makes providers reject the request.
    case 'assistant': return entry.message.stopReason === 'error' || entry.message.stopReason === 'aborted' ? undefined : entry.message satisfies AssistantMessage
    case 'toolResult':
      return {
        role: 'toolResult', toolCallId: entry.toolCallId, toolName: entry.toolName, content: entry.content, isError: entry.isError, timestamp: entry.ts,
      } as ToolResultMessage
    case 'compaction':
    case 'notice':
    case 'turn':
      return undefined
  }
}

/**
 * Build the conversation the model sees. A compaction entry replaces the items from `firstId` through `lastId`
 * (which may include an earlier compaction) with its framed summary.
 * @param entries Transcript entries in order.
 * @returns Context items in order, without the system message.
 */
export function buildContext(entries: readonly TranscriptEntry[]): ContextItem[] {
  const items: ContextItem[] = []
  for (const entry of entries) {
    if (entry.kind === 'compaction') {
      const first = items.findIndex(item => item.entryId === entry.firstId)
      const last = items.findIndex(item => item.entryId === entry.lastId)
      if (first < 0 || last < first) continue
      items.splice(first, last - first + 1, { entryId: entry.id, message: { role: 'user', content: frameSummary(entry.summary), timestamp: entry.ts } })
      continue
    }
    const message = entryMessage(entry)
    if (message !== undefined) items.push({ entryId: entry.id, message })
  }
  return items
}

/**
 * Whether a span can be cut out without separating a tool call from its result.
 * @param items Context items.
 * @param start First index of the span.
 * @param end Last index of the span (inclusive).
 * @returns Whether every tool call inside the span has its result inside the span and vice versa.
 */
export function toolPairsBalanced(items: readonly ContextItem[], start: number, end: number): boolean {
  const calls = new Set<string>()
  for (let index = start; index <= end; index++) {
    const message = items[index]!.message
    if (message.role === 'assistant') for (const block of message.content) if (block.type === 'toolCall') calls.add(block.id)
    if (message.role === 'toolResult') {
      if (!calls.has(message.toolCallId)) return false
      calls.delete(message.toolCallId)
    }
  }
  if (calls.size === 0) return true
  // Calls left open inside the span must not be answered after it.
  for (let index = end + 1; index < items.length; index++) {
    const message = items[index]!.message
    if (message.role === 'toolResult' && calls.has(message.toolCallId)) return false
  }
  return true
}
