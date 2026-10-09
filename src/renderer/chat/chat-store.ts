/** Client state of the chats the renderer shows, kept current from Host events. */
import { useEffect, useSyncExternalStore } from 'react'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { SessionSnapshot, SessionSummary, TranscriptEntry } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'

/** Live output of a running tool. */
export interface RunningTool { toolName: string; args?: unknown; partial?: string | undefined }

/** One chat as the renderer sees it. */
export interface ChatView extends SessionSnapshot {
  tools: Record<string, RunningTool>
  /** Entries before the loaded window exist on the Host. */
  truncated: boolean
  loading: boolean
}

/** Entries loaded when a chat opens; older ones load on demand. */
const INITIAL_ENTRIES = 400

const chats = new Map<string, ChatView>()
const listeners = new Map<string, Set<() => void>>()
const loading = new Map<string, Promise<void>>()

function publish(sessionId: string, view: ChatView): void {
  chats.set(sessionId, view)
  for (const listener of listeners.get(sessionId) ?? []) listener()
}

function update(sessionId: string, change: (view: ChatView) => ChatView): void {
  const view = chats.get(sessionId)
  if (view !== undefined) publish(sessionId, change(view))
}

/**
 * Load (or reload) a chat from the Host.
 * @param sessionId Chat id.
 * @param all Load every entry instead of the most recent ones.
 * @returns Completion.
 */
export function loadChat(sessionId: string, all = false): Promise<void> {
  const existing = loading.get(sessionId)
  if (existing !== undefined && !all) return existing
  const task = host.call('sessions.get', { sessionId, ...(all ? {} : { limit: INITIAL_ENTRIES }) }).then((snapshot) => {
    publish(sessionId, {
      ...snapshot, tools: Object.fromEntries(snapshot.runningTools.map(id => [id, { toolName: '' }])),
      truncated: !all && snapshot.entries.length >= INITIAL_ENTRIES, loading: false,
    })
  }).finally(() => { loading.delete(sessionId) })
  loading.set(sessionId, task)
  return task
}

host.on('session.entry', ({ sessionId, entry }) => {
  update(sessionId, view => view.entries.some(item => item.id === entry.id) ? view : { ...view, entries: [...view.entries, entry] })
})
host.on('session.stream', ({ sessionId, message }) => {
  update(sessionId, view => ({ ...view, streaming: message }))
})
host.on('session.tool', ({ sessionId, toolCallId, toolName, phase, args, partial }) => {
  update(sessionId, (view) => {
    const tools = { ...view.tools }
    if (phase === 'end') delete tools[toolCallId]
    else tools[toolCallId] = { toolName, args: args ?? tools[toolCallId]?.args, partial: partial ?? tools[toolCallId]?.partial }
    return { ...view, tools }
  })
})
host.on('session.state', ({ sessionId, status, queue, model, context, error }) => {
  update(sessionId, view => ({
    ...view, queue, model, context, error,
    summary: { ...view.summary, status },
    ...(status === 'idle' || status === 'error' ? { tools: {}, streaming: null } : {}),
  }))
})
host.on('sessions.changed', (summary) => {
  update(summary.id, view => ({ ...view, summary }))
})
host.onState(() => {
  if (host.state === 'open') for (const id of chats.keys()) void loadChat(id).catch(() => undefined)
})

/**
 * Follow one chat.
 * @param sessionId Chat id, or `null`.
 * @returns The chat view, or `undefined` while loading or when `sessionId` is `null`.
 */
export function useChat(sessionId: string | null): ChatView | undefined {
  useEffect(() => {
    if (sessionId !== null && !chats.has(sessionId)) void loadChat(sessionId).catch((error: unknown) => { console.error(error) })
  }, [sessionId])
  return useSyncExternalStore(
    (listener) => {
      if (sessionId === null) return () => undefined
      let set = listeners.get(sessionId)
      if (set === undefined) { set = new Set(); listeners.set(sessionId, set) }
      set.add(listener)
      return () => { set.delete(listener) }
    },
    () => sessionId === null ? undefined : chats.get(sessionId),
  )
}

/** Chat history list, kept current from `sessions.changed` and `sessions.removed`. */
let summaries: SessionSummary[] | undefined
const summaryListeners = new Set<() => void>()

function publishSummaries(next: SessionSummary[]): void {
  summaries = next.sort((left, right) => right.updatedAt - left.updatedAt)
  for (const listener of summaryListeners) listener()
}

/** Reload the history list. */
export function reloadSummaries(): Promise<void> {
  return host.call('sessions.list', {}).then(publishSummaries)
}

host.on('sessions.changed', (summary) => {
  if (summaries === undefined) return
  publishSummaries([summary, ...summaries.filter(item => item.id !== summary.id)])
})
host.on('sessions.removed', ({ sessionId }) => {
  if (summaries !== undefined) publishSummaries(summaries.filter(item => item.id !== sessionId))
  chats.delete(sessionId)
})
host.onState(() => { if (host.state === 'open' && summaries !== undefined) void reloadSummaries().catch(() => undefined) })

/** @returns Every chat summary; re-renders on change. */
export function useSummaries(): SessionSummary[] | undefined {
  useEffect(() => { if (summaries === undefined) void reloadSummaries().catch((error: unknown) => { console.error(error) }) }, [])
  return useSyncExternalStore(listener => { summaryListeners.add(listener); return () => { summaryListeners.delete(listener) } }, () => summaries)
}

/** Assistant output streaming into a turn, including partial tool calls. */
export type StreamingMessage = AssistantMessage

/**
 * Entries grouped into turns: a turn starts at a user entry and runs until the next one.
 * @param entries Chat entries in order.
 * @returns Groups; entries before the first user message form a leading group with `user` undefined.
 */
export function groupTurns(entries: readonly TranscriptEntry[]): { user: Extract<TranscriptEntry, { kind: 'user' }> | undefined; items: TranscriptEntry[] }[] {
  const groups: { user: Extract<TranscriptEntry, { kind: 'user' }> | undefined; items: TranscriptEntry[] }[] = []
  for (const entry of entries) {
    if (entry.kind === 'user') groups.push({ user: entry, items: [] })
    else {
      if (groups.length === 0) groups.push({ user: undefined, items: [] })
      groups[groups.length - 1]!.items.push(entry)
    }
  }
  return groups
}
