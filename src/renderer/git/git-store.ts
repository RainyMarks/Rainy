/** Git status of the open project for the Git pane, its tab badge and the status bar, kept current while the window is visible. */
import { useEffect, useSyncExternalStore } from 'react'
import type { GitStatus } from '../../shared/git-protocol.ts'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { onPageVisible, pageHidden } from '../app/visibility.ts'
import { host } from '../rpc.ts'

/** Polls while the Git pane is shown, and otherwise for the badge and branch. */
const FAST_MS = 3000
const SLOW_MS = 15_000
/** File and agent events in a row lead to one refresh. */
const SOON_MS = 400

/** The latest status of one project. */
export interface GitState {
  readonly workspaceId: WorkspaceId | null
  readonly status?: GitStatus | undefined
  readonly error?: string | undefined
}

let current: GitState = { workspaceId: null }
const listeners = new Set<() => void>()
let watchers = 0
let fastWatchers = 0
let timer: ReturnType<typeof setTimeout> | undefined
let soon: ReturnType<typeof setTimeout> | undefined
let inFlight: { readonly workspaceId: WorkspaceId; readonly done: Promise<void> } | undefined

function publish(next: GitState): void {
  current = next
  for (const listener of listeners) listener()
}

/**
 * Read the project's Git status now.
 * @returns Completion; failures are kept in the state.
 */
export function refreshGit(): Promise<void> {
  const workspaceId = current.workspaceId
  if (workspaceId === null) return Promise.resolve()
  if (inFlight?.workspaceId === workspaceId) return inFlight.done
  const done = host.call('git.status', { workspaceId })
    .then((status) => { if (current.workspaceId === workspaceId) publish({ workspaceId, status }) })
    .catch((error: unknown) => {
      if (current.workspaceId === workspaceId) publish({ ...current, error: error instanceof Error ? error.message : String(error) })
    })
    .finally(() => { if (inFlight?.done === done) { inFlight = undefined; schedule() } })
  inFlight = { workspaceId, done }
  return done
}

function schedule(): void {
  clearTimeout(timer)
  timer = undefined
  if (watchers === 0 || current.workspaceId === null || pageHidden()) return
  timer = setTimeout(() => { void refreshGit() }, fastWatchers > 0 ? FAST_MS : SLOW_MS)
}

/** Refresh shortly, after files changed. */
export function refreshGitSoon(): void {
  if (watchers === 0) return
  clearTimeout(soon)
  soon = setTimeout(() => { void refreshGit() }, SOON_MS)
}

if (typeof window !== 'undefined') {
  host.on('ide.hint', ({ kind }) => { if (kind === 'files') refreshGitSoon() })
  host.on('session.changes', () => { refreshGitSoon() })
  onPageVisible(() => { refreshGitSoon() })
  window.addEventListener('focus', () => { refreshGitSoon() })
}

/**
 * Follow the Git status of a project.
 * @param workspaceId Project, or `null` without one.
 * @param fast Poll often, for the Git pane itself.
 * @returns The latest status.
 */
export function useGitStatus(workspaceId: WorkspaceId | null, fast = false): GitState {
  useEffect(() => {
    if (current.workspaceId !== workspaceId) publish({ workspaceId })
    watchers++
    if (fast) fastWatchers++
    void refreshGit()
    return () => {
      watchers--
      if (fast) fastWatchers--
      schedule()
    }
  }, [workspaceId, fast])
  return useSyncExternalStore((listener) => { listeners.add(listener); return () => { listeners.delete(listener) } }, () => current)
}
