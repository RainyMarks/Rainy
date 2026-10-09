/** Bounded evidence projection for project memory; excludes reasoning and generated context. */
import { z } from 'zod'
import type { ProjectionDefinition } from '@deepseek-ai/dsh-session-projection'
import type { Message, MessageId, TokenUsage } from '@deepseek-ai/dsh-llm'
import { estimateText } from './budget.ts'
import { redactMemoryText } from './project-memory-store.ts'
import type { MemoryEvidence } from './project-memory-store.ts'

/** Exact auxiliary request, retained without changing the conversation surface. */
export interface ProjectMemoryRequestRecord {
  generationId: string
  projectId: string
  revision: number
  provider: string
  model: string
  system: string
  messages: Message[]
  maxTokens: number
  evidence: MemoryEvidence[]
}
/** Settlement of a memory call; failures preserve the preceding revision. */
export interface ProjectMemoryResultRecord {
  generationId: string
  status: 'committed' | 'unchanged' | 'cancelled' | 'failed'
  revision: number
  usage?: TokenUsage
  error?: string
}
/** Project revision linked to an ordinary cross-session recall message. */
export interface ProjectMemoryRecallRecord {
  messageId: MessageId
  projectId: string
  revision: number
}

declare module '@deepseek-ai/dsh-session/types' {
  interface SessionEventMap {
    /** Exact model-visible input of a project-memory auxiliary request. */
    'rainy/memory-request': ProjectMemoryRequestRecord
    /** Outcome and model usage for one project-memory request. */
    'rainy/memory-result': ProjectMemoryResultRecord
    /** Project-memory lineage for a separately logged recall message. */
    'rainy/memory-recall': ProjectMemoryRecallRecord
  }
}
const evidenceSchema = z.object({
  seq: z.number().int().nonnegative(),
  kind: z.enum(['user', 'assistant', 'tool']),
  text: z.string(),
  filePath: z.string().optional(),
})
const projectionSchema = z.object({
  evidence: z.array(evidenceSchema),
  calls: z.record(z.string(), z.object({ name: z.string(), filePath: z.string().optional() })),
  injectedRevision: z.number().int(),
  recallMessageId: z.string().nullable().default(null),
  recallRevision: z.number().int().default(-1),
  lastCompletedSeq: z.number().int(),
  lastGeneratedSeq: z.number().int(),
})
/** Host-only bounded notes input reconstructed on resume by the projection registry. */
export type MemoryProjectionState = z.infer<typeof projectionSchema>
declare module '@deepseek-ai/dsh-session-projection' {
  interface SessionProjectionStateMap {
    rainyMemory: MemoryProjectionState
  }
}

/**
 * Bound one source fragment without splitting Unicode code points.
 * @param text Source text.
 * @param tokens Conservative complete-text limit, including the omission marker.
 * @returns Bounded text; full evidence remains in its original session event.
 */
export function boundMemoryText(text: string, tokens: number): string {
  const clean = redactMemoryText(text)
  if (estimateText(clean) <= tokens) return clean
  const marker = '\n[Excerpt; remaining text stays in the source event.]'
  const points = Array.from(clean)
  let count = Math.min(points.length, tokens * 3)
  while (count > 0) {
    const value = points.slice(0, count).join('') + marker
    if (estimateText(value) <= tokens) return value
    count = Math.floor(count * 0.8)
  }
  return ''
}

/**
 * Build the deterministic bounded fold used by live and resumed sessions.
 * @param limits Maximum retained evidence count and each fragment's token budget.
 * @returns One host-only projection contribution.
 */
export function memoryProjection(limits: {
  evidenceItems: number
  evidenceItemTokens: number
}): ProjectionDefinition<'rainyMemory'> {
  return {
    key: 'rainyMemory',
    stateVersion: 1,
    stateSchema: projectionSchema,
    init: () => ({
      evidence: [],
      calls: {},
      injectedRevision: -1,
      recallMessageId: null,
      recallRevision: -1,
      lastCompletedSeq: -1,
      lastGeneratedSeq: -1,
    }),
    apply(state, event) {
      if (event.type === 'rainy/memory-recall') {
        return { ...state, recallMessageId: event.data.messageId, recallRevision: event.data.revision }
      }
      if (event.type === 'rainy/memory-request') {
        return {
          ...state,
          lastGeneratedSeq: Math.max(state.lastGeneratedSeq, ...event.data.evidence.map(item => item.seq)),
        }
      }
      if (event.type === 'turn/end' && event.data.reason.kind === 'completed')
        return { ...state, lastCompletedSeq: event.seq }
      if (event.type === 'tool/call') {
        let filePath: string | undefined
        if (['read', 'write', 'edit'].includes(event.data.name)) {
          try {
            const args: unknown = JSON.parse(event.data.arguments)
            if (args !== null && typeof args === 'object' && 'file_path' in args && typeof args.file_path === 'string')
              filePath = args.file_path
          } catch (_error) {
            /* Malformed tool arguments have no file evidence. */
          }
        }
        const calls = {
          ...state.calls,
          [event.data.callId]: { name: event.data.name, ...(filePath === undefined ? {} : { filePath }) },
        }
        return { ...state, calls: Object.fromEntries(Object.entries(calls).slice(-limits.evidenceItems)) }
      }
      let message: Message | undefined
      let kind: 'user' | 'assistant' | 'tool' | undefined
      let filePath: string | undefined
      let next = state
      if (event.type === 'user/message') {
        if (event.data.id === state.recallMessageId) return { ...state, injectedRevision: state.recallRevision }
        if (event.data.source.kind !== 'user') return state
        message = event.data
        kind = 'user'
      } else if (event.type === 'assistant/message' && !event.data.interrupted) {
        message = event.data.message
        kind = 'assistant'
      } else if (event.type === 'tool/result') {
        const calls = Object.fromEntries(
          Object.entries(state.calls).filter(([id]) => id !== event.data.message.toolCallId),
        )
        next = { ...state, calls }
        if (event.data.message.isError || !Object.hasOwn(state.calls, event.data.message.toolCallId)) return next
        const call = state.calls[event.data.message.toolCallId]
        message = event.data.message
        kind = 'tool'
        filePath = call.filePath
      }
      if (!message || !kind) return next
      const text = boundMemoryText(
        message.content
          .filter(block => block.type === 'text')
          .map(block => block.text)
          .join('\n'),
        limits.evidenceItemTokens,
      )
      if (!text) return next
      return {
        ...next,
        evidence: [
          ...next.evidence,
          { seq: event.seq, kind, text, ...(filePath === undefined ? {} : { filePath }) },
        ].slice(-limits.evidenceItems),
      }
    },
  }
}
