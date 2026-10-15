/** Project maps as the page holds them: loaded on demand, rescanned after the agent changes files, explained on request. */
import { useEffect, useSyncExternalStore } from 'react'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { ProjectMap } from '../../shared/map-protocol.ts'
import { host } from '../rpc.ts'

/** One project's map and what is happening to it. */
export interface MapState {
  readonly map?: ProjectMap | undefined
  readonly loading: boolean
  readonly explaining: boolean
  readonly error?: string | undefined
}

const IDLE: MapState = { loading: false, explaining: false }
/** Writes in a row lead to one rescan. */
const RESCAN_DELAY_MS = 2500

const states = new Map<WorkspaceId, MapState>()
const listeners = new Map<WorkspaceId, Set<() => void>>()
let rescan: ReturnType<typeof setTimeout> | undefined

function set(workspaceId: WorkspaceId, change: Partial<MapState>): void {
  states.set(workspaceId, { ...states.get(workspaceId) ?? IDLE, ...change })
  for (const listener of listeners.get(workspaceId) ?? []) listener()
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/**
 * Load a project's map.
 * @param workspaceId Project.
 * @param refresh Scan the project again instead of reusing the Host's recent map.
 */
export function loadMap(workspaceId: WorkspaceId, refresh = false): void {
  if (states.get(workspaceId)?.loading === true) return
  set(workspaceId, { loading: true, error: undefined })
  host.call('map.get', { workspaceId, ...(refresh ? { refresh } : {}) })
    .then((map) => { set(workspaceId, { map, loading: false }) })
    .catch((error: unknown) => { set(workspaceId, { loading: false, error: message(error) }) })
}

/**
 * Ask the AI to describe the project's parts.
 * @param workspaceId Project.
 * @returns Completion; rejects with the Host's message.
 */
export async function explainMap(workspaceId: WorkspaceId): Promise<void> {
  set(workspaceId, { explaining: true })
  try {
    const explanation = await host.call('map.explain', { workspaceId })
    const map = states.get(workspaceId)?.map
    set(workspaceId, { explaining: false, ...(map === undefined ? {} : { map: { ...map, explanation } }) })
  } catch (error) {
    set(workspaceId, { explaining: false })
    throw error
  }
}

// Files the agent writes appear on the maps being looked at.
host.on('session.changes', () => {
  clearTimeout(rescan)
  rescan = setTimeout(() => {
    for (const [workspaceId, group] of listeners) if (group.size > 0 && states.get(workspaceId)?.map !== undefined) loadMap(workspaceId, true)
  }, RESCAN_DELAY_MS)
})

/**
 * Follow a project's map, loading it the first time it is shown.
 * @param workspaceId Project, or `null`.
 * @param visible The map is on screen.
 * @returns The map state.
 */
export function useProjectMap(workspaceId: WorkspaceId | null, visible: boolean): MapState {
  useEffect(() => {
    if (workspaceId !== null && visible && states.get(workspaceId)?.map === undefined) loadMap(workspaceId)
  }, [workspaceId, visible])
  return useSyncExternalStore(
    (listener) => {
      if (workspaceId === null || !visible) return () => undefined
      let group = listeners.get(workspaceId)
      if (group === undefined) { group = new Set(); listeners.set(workspaceId, group) }
      group.add(listener)
      return () => { group.delete(listener) }
    },
    () => workspaceId === null ? IDLE : states.get(workspaceId) ?? IDLE,
  )
}
