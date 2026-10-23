/**
 * The streaming reply as the Host sends it to pages: a whole message, then only what each update adds to it (the Host's
 * `agent/stream.rs`). Text and reasoning grow at their end, so an update carries the appended characters; anything else
 * that changed is sent whole.
 */
import type { AssistantMessage } from './rpc.ts'

/** What one update adds to the message the page holds. */
export interface StreamPatch {
  /** Characters appended to a text or thinking block that held `at` characters. */
  readonly append?: readonly { readonly index: number; readonly at: number; readonly text: string }[]
  /** Blocks that are new or changed otherwise, whole. */
  readonly blocks?: readonly { readonly index: number; readonly block: AssistantMessage['content'][number] }[]
}
