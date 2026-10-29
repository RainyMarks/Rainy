/** What the agents are doing to project files right now, and what they did lately, from the chats' tool events. */
import { useSyncExternalStore } from 'react'
import { host } from '../rpc.ts'

/** One tool call of an agent, as the project map and the file tree show it. */
export interface AiActivity {
  readonly sessionId: string
  readonly toolCallId: string
  readonly action: 'read' | 'write' | 'edit' | 'search' | 'command' | 'other'
  /** Absolute path on the Host of the file or directory the call names, when it names one. */
  readonly path?: string | undefined
  /** What the call does, for the activity list: a command's description, a search pattern, or the tool name. */
  readonly label: string
  readonly startedAt: number
  /** Set once the call finished. */
  readonly endedAt?: number | undefined
}

/** A write or edit stays shown as in progress this long after it finished, so the change can be seen. */
export const LINGER_MS = 5000

/** Snapshot of the activity store. */
export interface AiActivityState {
  /**
   * Calls running now, oldest first: a call counts from the moment the model starts writing it (a file write takes
   * the model seconds to write and a moment to run), and a write or edit stays for {@link LINGER_MS} after it ends.
   */
  readonly active: readonly AiActivity[]
  /** Finished calls, newest first. */
  readonly recent: readonly AiActivity[]
}

/** Finished calls remembered. */
const RECENT_LIMIT = 80

const running = new Map<string, AiActivity>()
const lingering = new Map<string, AiActivity>()
let recent: AiActivity[] = []
let snapshot: AiActivityState = { active: [], recent: [] }
const listeners = new Set<() => void>()
const workingDirectories = new Map<string, string>()
let listing: Promise<void> | undefined

function publish(): void {
  snapshot = { active: [...running.values(), ...[...lingering.values()].filter(item => !running.has(item.toolCallId))], recent }
  for (const listener of listeners) listener()
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value : undefined
}

const ABSOLUTE = /^(?:[A-Za-z]:[\\/]|[\\/])/u

/**
 * Resolve a path a tool named against the chat's working directory, the way the Host does.
 * @param cwd Working directory, when known.
 * @param path Path from the tool call.
 * @returns An absolute path with the directory's separators, or the path as given when it cannot be resolved.
 */
export function resolveToolPath(cwd: string | undefined, path: string): string {
  if (ABSOLUTE.test(path) || cwd === undefined) return path
  const windows = /^[A-Za-z]:[\\/]/u.test(cwd) || cwd.includes('\\')
  const separator = windows ? '\\' : '/'
  const parts = cwd.split(/[\\/]+/u).filter((part, index) => part !== '' || index === 0)
  for (const part of path.split(/[\\/]+/u)) {
    if (part === '' || part === '.') continue
    if (part === '..') { if (parts.length > 1) parts.pop() } else parts.push(part)
  }
  const joined = parts.join(separator)
  return windows ? joined : joined.startsWith('/') ? joined : `/${joined}`
}

function describe(sessionId: string, toolCallId: string, toolName: string, args: unknown, cwd: string | undefined): AiActivity {
  const record = args !== null && typeof args === 'object' ? args as Record<string, unknown> : {}
  const base = { sessionId, toolCallId, startedAt: Date.now() }
  switch (toolName) {
    case 'read': case 'write': case 'edit': {
      const path = text(record.file_path)
      return { ...base, action: toolName, label: path ?? toolName, ...(path === undefined ? {} : { path: resolveToolPath(cwd, path) }) }
    }
    case 'grep': case 'glob': {
      const path = text(record.path)
      return { ...base, action: 'search', label: text(record.pattern) ?? toolName, path: resolveToolPath(cwd, path ?? '.') }
    }
    case 'bash': case 'pwsh':
      return { ...base, action: 'command', label: (text(record.description) ?? text(record.command) ?? toolName).slice(0, 200) }
    default:
      return { ...base, action: 'other', label: toolName.startsWith('mcp__') ? toolName.slice(5).replace('__', ': ') : toolName }
  }
}

/** Learn the working directories of chats that started before this page did. */
function learnDirectories(): Promise<void> {
  listing ??= host.call('sessions.list', {}).then((summaries) => {
    for (const summary of summaries) if (!workingDirectories.has(summary.id)) workingDirectories.set(summary.id, summary.cwd)
  }).catch(() => { listing = undefined })
  return listing
}

host.on('sessions.changed', (summary) => { workingDirectories.set(summary.id, summary.cwd) })
// The model names the file at the start of a call it is still writing.
host.on('session.stream', (event) => {
  const { sessionId } = event
  let changed = false
  // A tool call appears whole: in the first message of a reply or in a patch that adds or changes it.
  const blocks = 'patch' in event ? (event.patch.blocks ?? []).map(item => item.block) : event.message?.content ?? []
  for (const block of blocks) {
    if (block.type !== 'toolCall' || running.has(block.id) || (block.name !== 'write' && block.name !== 'edit' && block.name !== 'read')) continue
    if (text((block.arguments as Record<string, unknown> | undefined)?.file_path) === undefined) continue
    running.set(block.id, describe(sessionId, block.id, block.name, block.arguments, workingDirectories.get(sessionId)))
    changed = true
  }
  if (changed) publish()
})
host.on('session.tool', ({ sessionId, toolCallId, toolName, phase, args }) => {
  if (phase === 'start') {
    const cwd = workingDirectories.get(sessionId)
    running.set(toolCallId, describe(sessionId, toolCallId, toolName, args, cwd))
    publish()
    if (cwd === undefined) {
      void learnDirectories().then(() => {
        const current = running.get(toolCallId)
        const known = workingDirectories.get(sessionId)
        if (current === undefined || known === undefined) return
        running.set(toolCallId, describe(sessionId, toolCallId, toolName, args, known))
        publish()
      })
    }
  } else if (phase === 'end') {
    const finished = running.get(toolCallId)
    if (finished === undefined) return
    running.delete(toolCallId)
    const ended = { ...finished, endedAt: Date.now() }
    recent = [ended, ...recent].slice(0, RECENT_LIMIT)
    if (ended.action === 'write' || ended.action === 'edit') {
      lingering.set(toolCallId, ended)
      setTimeout(() => { if (lingering.delete(toolCallId)) publish() }, LINGER_MS)
    }
    publish()
  }
})
host.on('session.state', ({ sessionId, status }) => {
  if (status !== 'idle' && status !== 'error') return
  let changed = false
  for (const [id, activity] of running) if (activity.sessionId === sessionId) { running.delete(id); changed = true }
  if (changed) publish()
})

/** @returns The agents' running and recent tool calls; re-renders on change. */
export function useAiActivity(): AiActivityState {
  return useSyncExternalStore(listener => { listeners.add(listener); return () => { listeners.delete(listener) } }, () => snapshot)
}
