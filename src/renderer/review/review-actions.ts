/** Reviewing the agent's changes to one file in the editor: open the review, finish it, or undo the whole file. */
import type { EditorInstance } from '../ide/editor-types.ts'
import type { IdeModel } from '../ide/ide-model.ts'
import { fileLabel, keyFromAbsolute } from '../ide/ide-paths.ts'
import { host } from '../rpc.ts'
import { toast } from '../ui/index.ts'
import { reviewMessages } from './messages.ts'

/**
 * Open a file's changes from a chat as a review in the editor.
 * @param model IDE model of the window.
 * @param sessionId Chat of the changes.
 * @param path Absolute path on the Host.
 * @param toolCallIds Write and edit calls behind the changes.
 * @returns Completion; rejects when the Host cannot give the earlier text.
 */
export async function openReview(model: IdeModel, sessionId: string, path: string, toolCallIds: readonly string[]): Promise<void> {
  const workspace = model.state.getSnapshot().workspace
  const key = workspace === null ? undefined : keyFromAbsolute(workspace, path)
  if (key === undefined) {
    toast(reviewMessages.t('changesOutside'))
    return
  }
  const { base } = await host.call('chat.changeBase', { sessionId, path })
  await model.review(key, base, { sessionId, toolCallIds })
}

function reviewOf(model: IdeModel, path: string) {
  const buffer = model.state.getSnapshot().buffers[path]
  const review = buffer?.comparison?.kind === 'ai' ? buffer.comparison.review : undefined
  return buffer === undefined || review === undefined ? undefined : { buffer, review }
}

/**
 * Finish a review whose every change was kept or undone: save what the undone parts changed, and mark the changes as reviewed.
 * @param model IDE model.
 * @param path File under review.
 * @returns Completion.
 */
export async function finishReview(model: IdeModel, path: string): Promise<void> {
  const current = reviewOf(model, path)
  if (current === undefined) return
  if (current.buffer.dirty && !(await model.save(path))) return
  await host.call('chat.keep', { sessionId: current.review.sessionId, toolCallIds: [...current.review.toolCallIds] })
  model.endReview(path)
  toast(reviewMessages.t('reviewFileDone', { path: fileLabel(model.state.getSnapshot().workspace, path) }), { tone: 'success' })
}

/**
 * Undo every change of a review. A file the agent's last write still holds is put back by the Host, which also tells
 * the agent; otherwise the editor restores the earlier text and finishing the review saves it.
 * @param model IDE model.
 * @param editor The editor, for restoring the earlier text in place.
 * @param path File under review.
 * @returns Completion.
 */
export async function undoReview(model: IdeModel, editor: EditorInstance | undefined, path: string): Promise<void> {
  const current = reviewOf(model, path)
  if (current === undefined) return
  if (!current.buffer.dirty) {
    const result = await host.call('chat.revert', { sessionId: current.review.sessionId, toolCallIds: [...current.review.toolCallIds] })
    if (result.failed.length === 0 && result.reverted.length > 0) {
      model.endReview(path)
      // A file the agent created is gone after the undo.
      await model.reload(path).catch(() => { model.close(path) })
      toast(reviewMessages.t('reviewFileDone', { path: fileLabel(model.state.getSnapshot().workspace, path) }), { tone: 'success' })
      return
    }
  }
  await editor?.action('rainy.undoAll')
}
