/** Review of the agent's file changes: the files a chat changed, their text before, keeping and undoing them. */
import { readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { SessionSnapshot, TranscriptEntry } from '../../src/shared/rpc.ts'
import { ChatStore } from '../../src/host/agent/store.ts'
import { FakeOpenAI, harness, idle, tempHost } from './agent-fixtures.ts'

type ToolResultEntry = Extract<TranscriptEntry, { kind: 'toolResult' }>

function changeIds(snapshot: SessionSnapshot, name: string): string[] {
  return snapshot.entries.filter((entry): entry is ToolResultEntry => entry.kind === 'toolResult' && entry.toolName !== 'read')
    .filter(entry => JSON.stringify(entry.details).includes(name)).map(entry => entry.toolCallId)
}

describe('changes to review', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI | undefined
  beforeEach(() => { cleanups.length = 0 })
  afterEach(async () => {
    for (const cleanup of cleanups.reverse()) await cleanup()
    await server?.stop()
    server = undefined
  })

  it('lists changed files with their counts and earlier text, until they are kept or undone', async () => {
    server = new FakeOpenAI([
      { toolCalls: [{ name: 'read', arguments: { file_path: 'notes.txt' } }] },
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'notes.txt', old_string: 'alpha', new_string: 'beta' } }] },
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'notes.txt', old_string: 'beta', new_string: 'gamma\ndelta' } }] },
      { toolCalls: [{ name: 'write', arguments: { file_path: 'new/solve.py', content: 'print(1)\nprint(2)\n' } }] },
      { text: 'done' },
    ])
    await server.start()
    const h = await harness(server, cleanups)
    await writeFile(join(h.project, 'notes.txt'), '﻿alpha\r\nkeep\r\n')
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'change things' })
    await idle(h, chat.id)
    expect(h.events.filter(event => event.event === 'session.changes')).toHaveLength(3)

    const changes = await h.call('chat.changes', { sessionId: chat.id })
    expect(changes.map(change => [change.display, change.added, change.removed, change.created, change.comparable, change.undoable])).toEqual([
      ['notes.txt', 2, 1, false, true, true], ['new/solve.py', 2, 0, true, true, true],
    ])
    const snapshot = await h.call('sessions.get', { sessionId: chat.id })
    expect(changes[0]!.toolCallIds).toEqual(changeIds(snapshot, 'notes.txt'))
    expect(await h.call('chat.changeBase', { sessionId: chat.id, path: changes[0]!.path })).toEqual({ base: 'alpha\nkeep\n', current: 'gamma\ndelta\nkeep\n' })
    expect(await h.call('chat.changeBase', { sessionId: chat.id, path: changes[1]!.path })).toEqual({ base: '', current: 'print(1)\nprint(2)\n' })

    await h.call('chat.keep', { sessionId: chat.id, toolCallIds: changes[0]!.toolCallIds })
    expect((await h.call('chat.changes', { sessionId: chat.id })).map(change => change.display)).toEqual(['new/solve.py'])
    await expect(h.call('chat.changeBase', { sessionId: chat.id, path: changes[0]!.path })).rejects.toThrow(/not-found/u)
    expect(await readFile(join(h.project, 'notes.txt'), 'utf8')).toBe('﻿gamma\r\ndelta\r\nkeep\r\n')

    const undone = await h.call('chat.revert', { sessionId: chat.id, toolCallIds: changes[1]!.toolCallIds })
    expect(undone.failed).toEqual([])
    expect(await h.call('chat.changes', { sessionId: chat.id })).toEqual([])
  })

  it('leaves out a file the agent changed back', async () => {
    server = new FakeOpenAI([
      { toolCalls: [{ name: 'read', arguments: { file_path: 'a.txt' } }] },
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'a.txt', old_string: 'one', new_string: 'two' } }] },
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'a.txt', old_string: 'two', new_string: 'one' } }] },
      { text: 'done' },
    ])
    await server.start()
    const h = await harness(server, cleanups)
    await writeFile(join(h.project, 'a.txt'), 'one\n')
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'go' })
    await idle(h, chat.id)
    expect(await h.call('chat.changes', { sessionId: chat.id })).toEqual([])
  })
})

describe('kept changes on disk', () => {
  it('are read back with the chat and skipped by older readers', async () => {
    const { dir, cleanup } = await tempHost()
    try {
      const store = new ChatStore(join(dir, 'chats'))
      await store.load()
      const chat = await store.create({ workspaceId: null, cwd: dir })
      await store.keep(chat.id, ['call_1', 'call_2'])
      await store.keep(chat.id, [])
      expect((await store.read(chat.id)).kept).toEqual(['call_1', 'call_2'])
      const lines = (await readFile(join(dir, 'chats', `${chat.id}.jsonl`), 'utf8')).trim().split('\n')
      expect(JSON.parse(lines.at(-1)!)).toEqual({ type: 'review', kept: ['call_1', 'call_2'] })
    } finally {
      await cleanup()
    }
  })
})
