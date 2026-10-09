/** Busy tracking for target switches and update installs requested by the carrier. */

/** Collects "is anything running" probes and the freeze gate the carrier sets before restarting the Host. */
export class Activity {
  private readonly sources = new Set<() => boolean>()
  private frozen = false

  /**
   * Add a probe that reports whether its owner has running work.
   * @param probe Returns true while chats, runs, debug sessions or terminals are active.
   * @returns A function that removes the probe.
   */
  addSource(probe: () => boolean): () => void {
    this.sources.add(probe)
    return () => { this.sources.delete(probe) }
  }

  /** @returns Whether any registered owner has running work. */
  active(): boolean {
    for (const probe of this.sources) if (probe()) return true
    return false
  }

  /**
   * Handle the carrier's `inspect-activity` request.
   * @param mode `freeze` blocks new work when idle; `resume` lifts the block; `observe` only reports.
   * @returns Whether work was running at the time of the request.
   */
  inspect(mode: 'observe' | 'freeze' | 'resume'): boolean {
    const active = this.active()
    if (mode === 'resume') this.frozen = false
    else if (mode === 'freeze' && !active) this.frozen = true
    return active
  }

  /** @throws Error while the carrier is replacing this Host. */
  assertCanStart(): void {
    if (this.frozen) throw new Error('执行环境正在切换，请等待应用重新打开。')
  }
}
