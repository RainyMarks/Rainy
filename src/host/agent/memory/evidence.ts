/** Memory evidence from chat entries: user requests, assistant replies and successful tool results. */
import { isAbsolute, resolve } from 'node:path'
import { estimateText } from '../../../shared/budget.ts'
import type { TranscriptEntry } from '../../../shared/rpc.ts'
import { redactMemoryText } from './store.ts'

/** Evidence items kept per chat. */
export const EVIDENCE_ITEMS = 64
/** Tokens of one evidence item. */
export const EVIDENCE_ITEM_TOKENS = 256

/** One bounded fragment of a chat. `seq` is the entry's position in the chat file. */
export interface ChatEvidence {
  seq: number
  kind: 'user' | 'assistant' | 'tool'
  text: string
  filePath?: string | undefined
}

/**
 * Bound a fragment without splitting characters.
 * @param text Source text.
 * @param tokens Limit, including the omission marker.
 * @returns Redacted, bounded text.
 */
export function boundMemoryText(text: string, tokens: number): string {
  const clean = redactMemoryText(text)
  if (estimateText(clean) <= tokens) return clean
  const marker = '\n[Excerpt; remaining text stays in the source chat.]'
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
 * Collect evidence from a chat's entries up to its last completed turn.
 * @param entries Chat entries in file order.
 * @param cwd Chat working directory, for relative file paths.
 * @returns Evidence (most recent {@link EVIDENCE_ITEMS}) and the position of the last completed turn (-1 when none).
 */
export function collectEvidence(entries: readonly TranscriptEntry[], cwd: string): { evidence: ChatEvidence[]; lastCompletedSeq: number } {
  const lastCompletedSeq = entries.findLastIndex(entry => entry.kind === 'turn')
  const files = new Map<string, string>()
  const evidence: ChatEvidence[] = []
  for (let seq = 0; seq <= lastCompletedSeq; seq++) {
    const entry = entries[seq]!
    let kind: ChatEvidence['kind'] | undefined
    let text = ''
    let filePath: string | undefined
    if (entry.kind === 'user') { kind = 'user'; text = entry.text }
    else if (entry.kind === 'assistant') {
      for (const block of entry.message.content) {
        if (block.type === 'toolCall' && ['read', 'write', 'edit'].includes(block.name) && typeof block.arguments.file_path === 'string') {
          files.set(block.id, isAbsolute(block.arguments.file_path) ? block.arguments.file_path : resolve(cwd, block.arguments.file_path))
        }
      }
      if (entry.message.stopReason === 'error' || entry.message.stopReason === 'aborted') continue
      kind = 'assistant'
      text = entry.message.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')
    } else if (entry.kind === 'toolResult' && !entry.isError) {
      kind = 'tool'
      text = entry.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')
      filePath = files.get(entry.toolCallId)
    }
    if (kind === undefined) continue
    const bounded = boundMemoryText(text, EVIDENCE_ITEM_TOKENS)
    if (bounded !== '') evidence.push({ seq, kind, text: bounded, ...(filePath === undefined ? {} : { filePath }) })
  }
  return { evidence: evidence.slice(-EVIDENCE_ITEMS), lastCompletedSeq }
}
