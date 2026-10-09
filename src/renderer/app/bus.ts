/** In-page events between the IDE side and the AI pane. */
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'

/** Payloads of every workbench event. */
export interface WorkbenchEvents {
  /** Open a project file in the editor; `path` is absolute or relative to the primary root. */
  'editor.open': { path: string; workspaceId?: WorkspaceId | undefined; rootId?: IdeRootId | undefined; line?: number | undefined }
  /** Open assistant code in a read-only tab, optionally compared with the current file. */
  'editor.snippet': { code: string; language?: string | undefined; compare: boolean }
  /** Put text into the AI composer and send it (used by "Send selection to AI"). */
  'chat.send': { text: string; workspaceId: WorkspaceId }
  /** Show a pane of the window. */
  'pane.show': { pane: 'ai' | 'files' | 'bottom' | 'history' | 'settings' | 'ctf'; section?: string | undefined }
}

type Listener<E extends keyof WorkbenchEvents> = (payload: WorkbenchEvents[E]) => void
const listeners = new Map<keyof WorkbenchEvents, Set<Listener<never>>>()

/**
 * Publish a workbench event.
 * @param event Event name.
 * @param payload Event payload.
 */
export function emit<E extends keyof WorkbenchEvents>(event: E, payload: WorkbenchEvents[E]): void {
  for (const listener of listeners.get(event) ?? []) (listener as Listener<E>)(payload)
}

/**
 * Subscribe to a workbench event.
 * @param event Event name.
 * @param listener Receives the payload.
 * @returns A function that removes the listener.
 */
export function on<E extends keyof WorkbenchEvents>(event: E, listener: Listener<E>): () => void {
  let set = listeners.get(event)
  if (set === undefined) { set = new Set(); listeners.set(event, set) }
  set.add(listener as Listener<never>)
  return () => { set.delete(listener as Listener<never>) }
}
