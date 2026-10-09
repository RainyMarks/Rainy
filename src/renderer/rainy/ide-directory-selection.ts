/** One directory choice owns its open-or-attach action until both operations settle. */
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'

/** Coalesce repeated directory gestures while retaining the first gesture's action. */
export class IdeDirectorySelection {
  readonly pending = createSnapshotStore(false)
  private operation: Promise<void> | undefined
  private disposed = false

  /** @param options - native choice and the workspace operation that consumes its result. */
  constructor(private readonly options: {
    readonly choose: () => Promise<string | null>
    readonly adopt: (path: string, mode: 'open' | 'attach') => Promise<void>
  }) {}

  /**
   * Start one choice; overlapping requests share its result and cannot change its action.
   * @param mode - open a project or attach a root when this request starts.
   * @returns the shared choice and adoption completion; cancellation does not adopt a path.
   */
  open(mode: 'open' | 'attach' = 'open'): Promise<void> {
    if (this.disposed) return Promise.resolve()
    if (this.operation !== undefined) return this.operation
    const operation = Promise.resolve().then(async () => {
      if (this.isDisposed()) return
      const path = await this.options.choose()
      if (!this.isDisposed() && path !== null) await this.options.adopt(path, mode)
    }).finally(() => {
      this.operation = undefined
      if (!this.disposed) this.pending.set(false)
    })
    this.operation = operation
    this.pending.set(true)
    return operation
  }

  /** Ignore a late native choice after the workspace owner unloads. */
  dispose(): void { this.disposed = true }

  private isDisposed(): boolean { return this.disposed }
}
