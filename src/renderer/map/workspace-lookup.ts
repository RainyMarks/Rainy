/** The project a map page was opened for, found among the projects the Host knows. */
import { useEffect, useState } from 'react'
import type { IdeWorkspace, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { callIde } from '../ide/ide-api.ts'

/** Where finding the page's project stands. */
export type WorkspaceLookup =
  | { readonly status: 'loading' }
  | { readonly status: 'ready'; readonly workspace: IdeWorkspace }
  /** The Host's project list does not have it: the project was removed from Rainy after the page was opened. */
  | { readonly status: 'missing' }
  /** The list did not load; `retry` asks the Host again. */
  | { readonly status: 'failed'; readonly message: string; readonly retry: () => void }

const LOADING: WorkspaceLookup = { status: 'loading' }
const MISSING: WorkspaceLookup = { status: 'missing' }

/**
 * @param list The Host's projects.
 * @param workspaceId The project wanted.
 * @returns It, or that it is missing.
 */
export function lookupIn(list: readonly IdeWorkspace[], workspaceId: WorkspaceId): WorkspaceLookup {
  const workspace = list.find(item => item.workspaceId === workspaceId)
  return workspace === undefined ? MISSING : { status: 'ready', workspace }
}

/**
 * Find a project, again whenever the id changes or a failed attempt is retried; an answer for an earlier id or attempt
 * is dropped.
 * @param workspaceId The project wanted.
 * @returns Where finding it stands.
 */
export function useWorkspaceLookup(workspaceId: WorkspaceId): WorkspaceLookup {
  const [attempt, setAttempt] = useState(0)
  const [lookup, setLookup] = useState(LOADING)
  useEffect(() => {
    const controller = new AbortController()
    setLookup(LOADING)
    callIde({ op: 'workspaces.list' }, controller.signal).then((list) => {
      if (!controller.signal.aborted) setLookup(lookupIn(list, workspaceId))
    }, (error: unknown) => {
      if (controller.signal.aborted) return
      setLookup({ status: 'failed', message: error instanceof Error ? error.message : String(error), retry: () => { setAttempt(value => value + 1) } })
    })
    return () => { controller.abort() }
  }, [workspaceId, attempt])
  return lookup
}
