/** Undo the agent's file changes and tell the user what happened. */
import type { RevertResult } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { toast } from '../ui/index.ts'
import { chatT } from './messages.ts'

/**
 * @param sessionId Chat of the changes.
 * @param toolCallIds Write and edit calls to undo.
 * @returns The Host's answer, or `undefined` when the request failed.
 */
export async function revertChanges(sessionId: string, toolCallIds: readonly string[]): Promise<RevertResult | undefined> {
  try {
    const result = await host.call('chat.revert', { sessionId, toolCallIds: [...toolCallIds] })
    if (result.reverted.length > 0) toast(chatT('undoDone', { count: result.reverted.length }), { tone: 'success' })
    for (const failure of result.failed) {
      const reason = failure.reason === 'changed' ? chatT('undoChanged') : failure.reason === 'unavailable' ? chatT('undoUnavailable') : chatT('undoError')
      toast(chatT('undoFailed', { path: failure.path === '' ? '…' : failure.path, reason }))
    }
    return result
  } catch (error) {
    toast(error instanceof Error ? error.message : String(error))
    return undefined
  }
}
