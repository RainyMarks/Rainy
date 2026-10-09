/** Browser-owned first-message state; one submit owns Session creation and normal composer handoff. */
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import type { WorkspaceId } from '../ide-files-protocol.ts'

/** Unsent first-message contents; files remain browser objects until a Session exists. */
export interface DeferredDraftState {
  readonly text: string
  readonly files: readonly File[]
  readonly busy: boolean
  readonly error: string
}

/** Session creation and submission belong to the existing controllers supplied by the consumer. */
export interface DeferredDraftOptions {
  /** @returns the currently selected project, or null while no project is open. */
  readonly workspace: () => WorkspaceId | null
  /** @param workspaceId - captured target project. @param draft - captured browser contents.
   * @param adopt - release browser recovery ownership once the normal composer becomes visible.
   * @param signal - cancelled when the consumer unloads.
   * @returns completed normal-composer handoff.
   */
  readonly handoff: (workspaceId: WorkspaceId, draft: Pick<DeferredDraftState, 'text' | 'files'>,
    adopt: () => void, signal: AbortSignal) => Promise<void>
}

/** Publish an upload-ready draft through one uninterrupted reveal-and-submit action.
 * @param ready - normal attachment preparation; the hidden Session remains retained while it settles.
 * @param owner - navigation guard and normal composer actions; reveal transfers draft ownership.
 * @returns completion; supersession rejects before revealing, so the temporary Session draft may be released.
 */
export async function publishPreparedDraft(ready: Promise<void>, owner: {
  readonly assertCurrent: () => void
  readonly reveal: () => void
  readonly submit: () => void
  readonly fail: (error: unknown) => void
}): Promise<void> {
  let failure: { readonly error: unknown } | undefined
  try { await ready } catch (error: unknown) { failure = { error } }
  owner.assertCurrent()
  owner.reveal()
  if (failure === undefined) owner.submit()
  else owner.fail(failure.error)
}

/** Retain a first-message draft across temporary surfaces and coalesce repeated submission gestures. */
export class DeferredDraft {
  readonly state = createSnapshotStore<DeferredDraftState>({ text: '', files: [], busy: false, error: '' })
  private pending: Promise<void> | undefined
  private readonly lifetime = new AbortController()

  /** @param options - target selection and normal-controller handoff. */
  constructor(private readonly options: DeferredDraftOptions) {}

  /** @param text - replacement unsent text; ignored while a handoff owns it. */
  change(text: string): void {
    const current = this.state.getSnapshot()
    if (!current.busy && current.text !== text) this.state.set({ ...current, text })
  }

  /** @param files - browser file batch to retain in picker order. */
  addFiles(files: readonly File[]): void {
    const current = this.state.getSnapshot()
    if (!current.busy) this.state.set({ ...current, files: [...current.files, ...files] })
  }

  /** @param index - retained file position to remove. */
  removeFile(index: number): void {
    const current = this.state.getSnapshot()
    if (!current.busy) this.state.set({ ...current, files: current.files.filter((_, position) => position !== index) })
  }

  /** @returns the admitted handoff, shared by repeated gestures until it settles. */
  submit(): Promise<void> {
    if (this.lifetime.signal.aborted) return Promise.resolve()
    if (this.pending !== undefined) return this.pending
    const current = this.state.getSnapshot()
    const workspaceId = this.options.workspace()
    if (workspaceId === null || current.text.trim() === '' && current.files.length === 0) return Promise.resolve()
    this.state.set({ ...current, busy: true, error: '' })
    let adopted = false
    const task = Promise.resolve().then(() => {
      this.lifetime.signal.throwIfAborted()
      return this.options.handoff(workspaceId, { text: current.text, files: current.files }, () => {
        adopted = true
        this.state.set({ text: '', files: [], busy: true, error: '' })
      }, this.lifetime.signal)
    })
      .then(() => { this.state.set({ text: '', files: [], busy: false, error: '' }) }, (error: unknown) => {
        if (adopted) this.state.set({ text: '', files: [], busy: false, error: '' })
        else this.state.set({ ...current, busy: false,
          error: error instanceof DOMException && error.name === 'AbortError' ? '' : error instanceof Error ? error.message : String(error) })
      }).finally(() => { if (this.pending === task) this.pending = undefined })
    this.pending = task
    return task
  }

  /** Cancel late continuations and wait for the admitted handoff to release its Session reference.
   * @returns completion of all previously admitted work.
   */
  async dispose(): Promise<void> {
    this.lifetime.abort()
    await this.pending
  }
}
