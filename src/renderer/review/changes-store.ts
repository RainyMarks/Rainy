/** The files each chat's agent changed that the user has not kept or undone yet, kept current from Host events. */
import { useEffect, useSyncExternalStore } from 'react'
import type { ChatChange } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'

const EMPTY: readonly ChatChange[] = []
/** Several writes in a row are listed once. */
const REFRESH_DELAY_MS = 150

const lists = new Map<string, readonly ChatChange[]>()
const listeners = new Map<string, Set<() => void>>()
const timers = new Map<string, ReturnType<typeof setTimeout>>()

function publish(sessionId: string, list: readonly ChatChange[]): void {
  lists.set(sessionId, list)
  for (const listener of listeners.get(sessionId) ?? []) listener()
}

/**
 * Load a chat's changed files now.
 * @param sessionId Chat.
 * @returns Completion; rejects when the Host cannot list them.
 */
export async function refreshChanges(sessionId: string): Promise<void> {
  publish(sessionId, await host.call('chat.changes', { sessionId }))
}

function schedule(sessionId: string): void {
  clearTimeout(timers.get(sessionId))
  timers.set(sessionId, setTimeout(() => {
    timers.delete(sessionId)
    void refreshChanges(sessionId).catch(() => undefined)
  }, REFRESH_DELAY_MS))
}

host.on('session.changes', ({ sessionId }) => { if ((listeners.get(sessionId)?.size ?? 0) > 0) schedule(sessionId) })
host.onState(() => {
  if (host.state !== 'open') return
  for (const [sessionId, set] of listeners) if (set.size > 0) schedule(sessionId)
})

/**
 * Follow one chat's changed files.
 * @param sessionId Chat, or `null` for none.
 * @returns The files, in the order the agent first changed them.
 */
export function useChanges(sessionId: string | null): readonly ChatChange[] {
  useEffect(() => {
    if (sessionId !== null) void refreshChanges(sessionId).catch(() => undefined)
  }, [sessionId])
  return useSyncExternalStore(
    (listener) => {
      if (sessionId === null) return () => undefined
      let set = listeners.get(sessionId)
      if (set === undefined) { set = new Set(); listeners.set(sessionId, set) }
      set.add(listener)
      return () => { set.delete(listener) }
    },
    () => sessionId === null ? EMPTY : lists.get(sessionId) ?? EMPTY,
  )
}
