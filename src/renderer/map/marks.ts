/** Which project files the agent is reading or writing now, and which it changed without review, by editor key. */
import type { IdeWorkspace } from '../../shared/ide-files-protocol.ts'
import type { ChatChange } from '../../shared/rpc.ts'
import { keyFromAbsolute } from '../ide/ide-paths.ts'
import type { AiActivity } from './activity.ts'

/** How the agent touches a file: running a read or a write now, or a change waiting for review. */
export type AiMark = 'reading' | 'writing' | 'changed' | 'created'

/**
 * Marks for the files of the current project.
 * @param workspace Current project.
 * @param changes Files changed in the current chat and not yet reviewed.
 * @param active Tool calls running in any chat.
 * @returns Mark per editor key; a running write outranks a pending change, which outranks a running read.
 */
export function aiMarks(workspace: IdeWorkspace | null, changes: readonly ChatChange[], active: readonly AiActivity[]): ReadonlyMap<string, AiMark> {
  const marks = new Map<string, AiMark>()
  if (workspace === null) return marks
  for (const activity of active) {
    if (activity.path === undefined || activity.action !== 'read') continue
    const key = keyFromAbsolute(workspace, activity.path)
    if (key !== undefined) marks.set(key, 'reading')
  }
  for (const change of changes) {
    const key = keyFromAbsolute(workspace, change.path)
    if (key !== undefined && !change.deleted) marks.set(key, change.created ? 'created' : 'changed')
  }
  for (const activity of active) {
    if (activity.path === undefined || (activity.action !== 'write' && activity.action !== 'edit')) continue
    const key = keyFromAbsolute(workspace, activity.path)
    if (key !== undefined) marks.set(key, 'writing')
  }
  return marks
}

/**
 * The directories holding marked files, so a collapsed folder still shows that something inside is marked.
 * @param marks File marks.
 * @returns Directory keys.
 */
export function markedDirectories(marks: ReadonlyMap<string, AiMark>): ReadonlySet<string> {
  const directories = new Set<string>()
  for (const key of marks.keys()) {
    let slash = key.indexOf('/')
    while (slash > 0) {
      directories.add(key.slice(0, slash))
      slash = key.indexOf('/', slash + 1)
    }
  }
  return directories
}
