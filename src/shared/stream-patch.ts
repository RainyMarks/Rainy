/**
 * The streaming reply as the Host sends it to the page: a whole message, then only what each update adds to it. Text and
 * reasoning grow at their end, so an update carries the appended characters; anything else that changed is sent whole.
 */
import type { AssistantMessage } from '@earendil-works/pi-ai'

type Block = AssistantMessage['content'][number]

/** What one update adds to the message the page holds. */
export interface StreamPatch {
  /** Characters appended to a text or thinking block that held `at` characters. */
  readonly append?: readonly { readonly index: number; readonly at: number; readonly text: string }[]
  /** Blocks that are new or changed otherwise, whole. */
  readonly blocks?: readonly { readonly index: number; readonly block: Block }[]
}

/** The sender's record of what the page received, to compute the next patch from. */
export interface StreamBaseline {
  /** The message without its content. */
  readonly rest: string
  readonly blocks: readonly {
    readonly type: Block['type']
    /** Characters of streamed text, for text and thinking blocks. */
    readonly length: number
    /** The last characters of that text, to notice a provider that rewrote rather than appended. */
    readonly tail: string
    /** The block without its streamed text (all of it for tool calls). */
    readonly other: string
  }[]
}

const TAIL = 16
/** Fields of a tool call the page reads; providers add scratch fields (`partialJson`, `partialArgs`, …) while it streams. */
const TOOL_CALL_FIELDS = new Set(['type', 'id', 'name', 'arguments', 'thoughtSignature', 'namespace'])
/** Arguments named early in a call that the page shows while the rest is still being written. */
const PREVIEW = /"(file_path|path|command|pattern|query|url|description)"\s*:\s*"((?:[^"\\]|\\.)*)"/gu
/** Only the start of the partial arguments is searched; a file's content follows its path. */
const PREVIEW_CHARS = 4096

function streamed(block: Block): string | undefined {
  return block.type === 'text' ? block.text : block.type === 'thinking' ? block.thinking : undefined
}

function previewArguments(partial: string): Record<string, string> {
  const preview: Record<string, string> = {}
  for (const match of partial.slice(0, PREVIEW_CHARS).matchAll(PREVIEW)) {
    try { preview[match[1]!] ??= JSON.parse(`"${match[2]!}"`) as string } catch (_invalidEscape) { /* Shown once the call is complete. */ }
  }
  return preview
}

/**
 * The streaming reply as the page receives it: tool calls without their providers' scratch fields, and with the
 * arguments named so far while the arguments are still being written (they are parsed only when the call is complete).
 * @param message The reply so far, which the provider keeps changing; not changed here.
 * @returns A copy fit to send, which keeps the text as it is now.
 */
export function compactStreaming(message: AssistantMessage): AssistantMessage {
  return {
    ...message,
    usage: { ...message.usage, cost: { ...message.usage.cost } },
    content: message.content.map((block): Block => {
      if (block.type !== 'toolCall') return { ...block }
      const scratch = block as unknown as Record<string, unknown>
      const kept = Object.fromEntries(Object.entries(scratch).filter(([key]) => TOOL_CALL_FIELDS.has(key))) as unknown as typeof block
      const partial = [scratch.partialJson, scratch.partialArgs, scratch.customInput].find((value): value is string => typeof value === 'string')
      const empty = kept.arguments === undefined || Object.keys(kept.arguments).length === 0
      return partial !== undefined && empty ? { ...kept, arguments: previewArguments(partial) } : kept
    }),
  }
}

/**
 * Record what the page holds after receiving `message`.
 * @param message The message sent (whole or as a patch).
 * @returns Its baseline.
 */
export function streamBaseline(message: AssistantMessage): StreamBaseline {
  const { content, ...rest } = message
  return {
    rest: JSON.stringify(rest),
    blocks: content.map((block) => {
      const text = streamed(block)
      if (text === undefined) return { type: block.type, length: 0, tail: '', other: JSON.stringify(block) }
      const { [block.type === 'text' ? 'text' : 'thinking']: _streamed, ...other } = block as unknown as Record<string, unknown>
      return { type: block.type, length: text.length, tail: text.slice(-TAIL), other: JSON.stringify(other) }
    }),
  }
}

/**
 * What changed since the baseline.
 * @param baseline What the page holds.
 * @param message The reply now.
 * @returns The patch (empty when nothing changed, `undefined` when only the whole message describes the change) and the
 * baseline of `message`, which the page holds once it applied either.
 */
export function streamPatch(baseline: StreamBaseline, message: AssistantMessage): { patch: StreamPatch | undefined; baseline: StreamBaseline } {
  const next = streamBaseline(message)
  return { patch: diff(baseline, next, message), baseline: next }
}

function diff(baseline: StreamBaseline, next: StreamBaseline, message: AssistantMessage): StreamPatch | undefined {
  if (next.rest !== baseline.rest || next.blocks.length < baseline.blocks.length) return undefined
  const append: { index: number; at: number; text: string }[] = []
  const blocks: { index: number; block: Block }[] = []
  for (const [index, block] of message.content.entries()) {
    const before = baseline.blocks[index]
    const now = next.blocks[index]!
    if (before === undefined) { blocks.push({ index, block }); continue }
    if (before.type !== now.type) return undefined
    const text = streamed(block)
    if (text === undefined) {
      if (now.other !== before.other) blocks.push({ index, block })
      continue
    }
    if (now.other !== before.other || text.length < before.length || text.slice(Math.max(0, before.length - TAIL), before.length) !== before.tail) {
      blocks.push({ index, block })
    } else if (text.length > before.length) append.push({ index, at: before.length, text: text.slice(before.length) })
  }
  return { ...(append.length === 0 ? {} : { append }), ...(blocks.length === 0 ? {} : { blocks }) }
}

/**
 * Apply a patch to the message the page holds.
 * @param message The page's copy, or `null` when it has none.
 * @param patch The update.
 * @returns The new message, or `undefined` when the patch does not fit this copy (the next whole message repairs it).
 */
export function applyStreamPatch(message: AssistantMessage | null, patch: StreamPatch): AssistantMessage | undefined {
  if (message === null) return undefined
  const content = [...message.content]
  for (const { index, block } of patch.blocks ?? []) {
    if (index > content.length) return undefined
    content[index] = block
  }
  for (const { index, at, text } of patch.append ?? []) {
    const block = content[index]
    const current = block === undefined ? undefined : streamed(block)
    if (block === undefined || current === undefined || current.length !== at) return undefined
    content[index] = block.type === 'text' ? { ...block, text: current + text } : { ...block, thinking: current + text } as Block
  }
  return { ...message, content }
}
