import { appendFile, readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { ChatStore } from '../../src/host/agent/store.ts'
import type { TranscriptEntry } from '../../src/shared/rpc.ts'
import { tempHost } from './agent-fixtures.ts'

function notice(id: string, text: string): TranscriptEntry {
  return { id, kind: 'notice', ts: 1, level: 'info', text }
}

describe('chat files', () => {
  it('stay readable after a crash left a partial last line', async () => {
    const { dir, cleanup } = await tempHost()
    try {
      const store = new ChatStore(join(dir, 'chats'))
      await store.load()
      const chat = await store.create({ workspaceId: null, cwd: dir })
      await store.append(chat.id, [notice('a', 'before the crash')])
      await store.flush()
      const file = join(dir, 'chats', `${chat.id}.jsonl`)
      // The process ended in the middle of writing a record.
      await appendFile(file, '{"type":"entry","entry":{"id":"b","kind":"no')

      // The next run appends after the damage, then the chat is read again.
      const restarted = new ChatStore(join(dir, 'chats'))
      await restarted.load()
      await restarted.append(chat.id, [notice('c', 'after the restart')])
      await restarted.setMeta(chat.id, { title: 'Recovered' })
      await restarted.append(chat.id, [notice('d', 'later still')])
      const read = await new ChatStore(join(dir, 'chats')).read(chat.id)
      expect(read.entries.map(entry => entry.id)).toEqual(['a', 'c', 'd'])
      expect(read.chat.title).toBe('Recovered')
      expect((await readFile(file, 'utf8')).split('\n').filter(line => line.startsWith('{"type":"entry","entry":{"id":"b"'))).toHaveLength(1)
      expect(await restarted.search('later still', 5)).toHaveLength(1)
    } finally {
      await cleanup()
    }
  })
})
