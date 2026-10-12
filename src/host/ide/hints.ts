/**
 * Change hints for the page: a file in a directory of an open editor tab changed, or a run produced events. The page
 * reads the actual state with its usual requests, so a lost hint only delays an update until its slow fallback poll.
 */
import { watch, type FSWatcher } from 'node:fs'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'

/** What changed. */
export type IdeHintKind = 'files' | 'execution'

/** Hints of one kind and workspace are sent at most this often. */
const HINT_INTERVAL_MS = 100
/** Directories watched per project folder; open tabs beyond them rely on the fallback poll. */
const MAX_WATCHED_DIRECTORIES = 64

interface WatchSet {
  readonly watchers: Map<string, FSWatcher>
  lease?: NodeJS.Timeout | undefined
}

/** Throttled hints and the directory watchers behind file hints. */
export class IdeHints {
  private readonly timers = new Map<string, { timer: NodeJS.Timeout; pending: boolean }>()
  private readonly sets = new Map<string, WatchSet>()
  private closed = false

  /**
   * @param emit Sends one hint to the page.
   * @param leaseMs Watchers of a project folder close when the page has not checked its files for this long: its tabs
   *   were closed, it switched projects, or its window is hidden. On Windows a watched folder cannot be renamed.
   */
  constructor(private readonly emit: (workspaceId: WorkspaceId, kind: IdeHintKind) => void, private readonly leaseMs: number) {}

  /**
   * Send a hint now, or once at the end of the current interval when one was sent recently.
   * @param workspaceId Project.
   * @param kind What changed.
   */
  notify(workspaceId: WorkspaceId, kind: IdeHintKind): void {
    if (this.closed) return
    const key = `${kind}:${workspaceId}`
    const current = this.timers.get(key)
    if (current !== undefined) { current.pending = true; return }
    this.emit(workspaceId, kind)
    const entry = {
      pending: false,
      timer: setTimeout(() => {
        this.timers.delete(key)
        if (entry.pending) this.notify(workspaceId, kind)
      }, HINT_INTERVAL_MS),
    }
    entry.timer.unref()
    this.timers.set(key, entry)
  }

  /**
   * Watch exactly these directories for one folder of a project, replacing its previous set, until the lease ends.
   * @param workspaceId Project.
   * @param rootId Project folder the page checked.
   * @param directories Canonical directories of the files open in its tabs.
   */
  watchDirectories(workspaceId: WorkspaceId, rootId: IdeRootId, directories: readonly string[]): void {
    if (this.closed) return
    const key = `${workspaceId}\u0000${rootId}`
    const wanted = new Set([...new Set(directories)].slice(0, MAX_WATCHED_DIRECTORIES))
    const set: WatchSet = this.sets.get(key) ?? { watchers: new Map() }
    clearTimeout(set.lease)
    for (const [directory, watcher] of set.watchers) {
      if (!wanted.has(directory)) { watcher.close(); set.watchers.delete(directory) }
    }
    for (const directory of wanted) {
      if (set.watchers.has(directory)) continue
      try {
        const watcher = watch(directory, { persistent: false }, () => { this.notify(workspaceId, 'files') })
        // A directory that disappears or cannot be watched falls back to polling.
        watcher.on('error', () => { watcher.close(); set.watchers.delete(directory) })
        set.watchers.set(directory, watcher)
      } catch (_unwatchable) {
        // Network shares and some WSL mounts cannot be watched; the fallback poll covers them.
      }
    }
    if (set.watchers.size === 0) { this.sets.delete(key); return }
    set.lease = setTimeout(() => { this.release(key) }, this.leaseMs)
    set.lease.unref()
    this.sets.set(key, set)
  }

  /** Stop every watcher and timer. */
  close(): void {
    this.closed = true
    for (const entry of this.timers.values()) clearTimeout(entry.timer)
    this.timers.clear()
    for (const key of [...this.sets.keys()]) this.release(key)
  }

  private release(key: string): void {
    const set = this.sets.get(key)
    if (set === undefined) return
    clearTimeout(set.lease)
    for (const watcher of set.watchers.values()) watcher.close()
    this.sets.delete(key)
  }
}
