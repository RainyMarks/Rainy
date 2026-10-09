/** Context compaction: replace an older span of a chat's context with a model-written checkpoint. */
import { randomUUID } from 'node:crypto'
import { createInitialSystemMessage, type Message, type Tool } from '@earendil-works/pi-ai'
import { estimateRequest, resolveBudget, type Budget, type CountableRequest } from '../../shared/budget.ts'
import type { TranscriptEntry } from '../../shared/rpc.ts'
import { toolPairsBalanced, type ContextItem } from './context.ts'
import { reasoningOption } from './llm.ts'
import type { Models, ResolvedModel } from './models.ts'

type CompactionEntry = Extract<TranscriptEntry, { kind: 'compaction' }>

/**
 * Estimate the tokens of a request.
 * @param system System prompt.
 * @param tools Tool declarations.
 * @param messages Conversation messages.
 * @returns Estimated input tokens.
 */
export function estimateMessages(system: string, tools: readonly Tool[], messages: readonly Message[]): number {
  const request: CountableRequest = {
    system,
    tools: tools.map(tool => ({ name: tool.name, description: tool.description, parameters: tool.parameters })),
    messages: messages.map(message => ({
      role: message.role,
      content: typeof message.content === 'string' ? [{ type: 'text', text: message.content }] : message.content,
    })),
  }
  return estimateRequest(request)
}

/**
 * Request estimate at which a chat compacts before sending.
 * @param budget Model budget.
 * @returns Token threshold.
 */
export function compactionThreshold(budget: Budget): number {
  return budget.compactAt
}

function directive(summaryTokens: number): string {
  return `You are the compaction engine. Write only a progress checkpoint, aiming for at most ${Math.floor(summaryTokens / 2)} tokens and staying below ${summaryTokens} tokens. `
    + 'Use terse prose, no headings or tools. Direct user messages are authoritative reference and remain verbatim outside this checkpoint. '
    + 'Summarize only verified completed progress, exact file paths and sequence or range progress, errors, unresolved work, and the next action. '
    + 'Do not invent, redefine, or restate user goals or constraints. If a prior checkpoint conflicts with direct user instructions, discard that claim. '
    + 'Merge still-valid progress without copying stale claims. Never claim unverified success.'
}

function itemTokens(item: ContextItem): number {
  return estimateMessages('', [], [item.message]) - 16
}

/**
 * Choose the span to compact: the part of the context older than the most recent `keepRecentTokens`, never including
 * the most recent user request, and never separating a tool call from its result.
 * @param items Context items (without the system message).
 * @param budget Model budget.
 * @param latestUser Entry id of the most recent user request, kept verbatim.
 * @param manual Use the largest eligible span regardless of the recent-tokens reserve.
 * @returns Inclusive index range, or `undefined` when nothing can be compacted.
 */
export function selectSpan(items: readonly ContextItem[], budget: Budget, latestUser: string | undefined, manual: boolean): [number, number] | undefined {
  const userIndex = latestUser === undefined ? -1 : items.findIndex(item => item.entryId === latestUser)
  // The most recent items that stay as they are.
  let keepFrom = items.length
  if (!manual) {
    let kept = 0
    while (keepFrom > 0 && kept + itemTokens(items[keepFrom - 1]!) <= budget.keepRecentTokens) kept += itemTokens(items[--keepFrom]!)
  } else keepFrom = items.length - 1
  const candidates: [number, number][] = []
  if (userIndex > 0) candidates.push([0, Math.min(userIndex - 1, keepFrom - 1)])
  candidates.push([userIndex + 1, keepFrom - 1])
  if (userIndex < 0) candidates.splice(0, candidates.length, [0, keepFrom - 1])
  for (const [start, initialEnd] of candidates) {
    let end = initialEnd
    while (end > start && !toolPairsBalanced(items, start, end)) end--
    if (end <= start) continue
    const tokens = items.slice(start, end + 1).reduce((total, item) => total + itemTokens(item), 0)
    if (tokens > budget.summaryTokens + 512) return [start, end]
  }
  return undefined
}

/** Inputs of one compaction. */
export interface CompactionRequest {
  models: Models
  resolved: ResolvedModel
  system: string
  tools: readonly Tool[]
  items: readonly ContextItem[]
  latestUser: string | undefined
  trigger: CompactionEntry['trigger']
  signal?: AbortSignal | undefined
}

/**
 * Summarize one span of the context.
 * @param request Model, context and trigger.
 * @returns The compaction entry, or `undefined` when there is nothing worth compacting.
 * @throws Error when the summary request fails.
 */
export async function compact(request: CompactionRequest): Promise<CompactionEntry | undefined> {
  const budget = resolveBudget(request.resolved.setup.contextWindow, request.resolved.setup.maxTokens)
  const span = selectSpan(request.items, budget, request.latestUser, request.trigger === 'manual')
  if (span === undefined) return undefined
  let [start, end] = span
  const summaryBudget = resolveBudget(budget.contextWindow, budget.summaryTokens)
  const fixed = estimateMessages(request.system, request.tools, []) + 256
  // A span larger than one summary request can hold is shortened from its start; later compactions take the rest.
  while (start < end && fixed + request.items.slice(start, end + 1).reduce((total, item) => total + itemTokens(item), 0) > summaryBudget.inputLimit) start++
  while (start < end && !toolPairsBalanced(request.items, start, end)) start++
  if (start >= end) return undefined
  const span_ = request.items.slice(start, end + 1)
  const tokensBefore = estimateMessages(request.system, request.tools, request.items.map(item => item.message))
  const system = createInitialSystemMessage(request.system, request.tools.map(tool => ({ name: tool.name, description: tool.description, parameters: tool.parameters })))
  const messages: Message[] = [...(system === undefined ? [] : [system]), ...span_.map(item => item.message), { role: 'user', content: directive(budget.summaryTokens), timestamp: Date.now() }]
  const stream = request.models.streamFn(request.resolved.model, { messages } as never, {
    maxTokens: budget.summaryTokens,
    toolChoice: 'none',
    ...(request.signal === undefined ? {} : { signal: request.signal }),
    ...(reasoningOption(request.resolved.thinking) === undefined ? {} : { reasoning: reasoningOption(request.resolved.thinking) }),
  })
  const reply = await stream.result()
  if (reply.stopReason === 'error' || reply.stopReason === 'aborted') throw new Error(reply.errorMessage ?? 'Compaction request failed.')
  const summary = reply.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('').trim()
  if (summary === '') throw new Error('Compaction returned an empty checkpoint.')
  return {
    id: randomUUID(), kind: 'compaction', ts: Date.now(),
    firstId: span_[0]!.entryId, lastId: span_.at(-1)!.entryId, summary, tokensBefore, trigger: request.trigger,
  }
}
