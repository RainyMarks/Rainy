/** Chats on disk: `chats/<id>.jsonl` (header line, then entries and metadata lines) and `chats/index.json`. */
import { randomUUID } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { appendFile, mkdir, readdir, readFile, rm, stat } from 'node:fs/promises'
import { join } from 'node:path'
import { createInterface } from 'node:readline'
import { z } from 'zod'
import { brandString } from '../../shared/brand.ts'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { ModelSelection, SessionId, TranscriptEntry } from '../../shared/rpc.ts'
import { readJson, SerialQueue, writeFileAtomic, writeJson } from '../files.ts'

/** Format version of chat files. */
export const CHAT_FORMAT_VERSION = 1

/** Persisted summary of one chat; run status is added by the session manager. */
export interface StoredChat {
  id: SessionId
  workspaceId: WorkspaceId | null
  cwd: string
  title: string
  createdAt: number
  updatedAt: number
  archived: boolean
  pinned: boolean
  model: ModelSelection | null
  parent?: { sessionId: SessionId; entryId: string } | undefined
}

/** Metadata changes recorded as `meta` lines; later lines win. */
export type ChatMetaPatch = Partial<Pick<StoredChat, 'title' | 'archived' | 'pinned' | 'model'>>

const headerSchema = z.object({
  type: z.literal('header'),
  version: z.literal(CHAT_FORMAT_VERSION),
  id: z.string().min(1),
  workspaceId: z.string().nullable(),
  cwd: z.string(),
  createdAt: z.number(),
  parent: z.object({ sessionId: z.string(), entryId: z.string() }).optional(),
})
const selectionSchema = z.object({ provider: z.string(), model: z.string(), thinking: z.enum(['off', 'low', 'high', 'max']).optional() })
const metaSchema = z.object({
  type: z.literal('meta'),
  title: z.string().optional(),
  archived: z.boolean().optional(),
  pinned: z.boolean().optional(),
  model: selectionSchema.nullable().optional(),
})
const indexSchema = z.object({
  version: z.literal(1),
  chats: z.array(z.object({
    id: z.string(), workspaceId: z.string().nullable(), cwd: z.string(), title: z.string(), createdAt: z.number(), updatedAt: z.number(),
    archived: z.boolean(), pinned: z.boolean(), model: selectionSchema.nullable(),
    parent: z.object({ sessionId: z.string(), entryId: z.string() }).optional(),
  })),
})

/** A chat file's content. */
export interface ChatFile {
  chat: StoredChat
  entries: TranscriptEntry[]
}

const ID_PATTERN = /^[A-Za-z0-9-]{1,64}$/

/** Reads and writes chat files; one instance per Host. */
export class ChatStore {
  private readonly chats = new Map<SessionId, StoredChat>()
  private readonly queues = new Map<SessionId, SerialQueue>()
  private readonly indexQueue = new SerialQueue()
  private indexTimer: NodeJS.Timeout | undefined

  /** @param dir `<home>/chats`. */
  constructor(private readonly dir: string) {}

  /** Load the index, rebuilding it from the chat files when it is missing or unreadable. */
  async load(): Promise<void> {
    await mkdir(this.dir, { recursive: true, mode: 0o700 })
    try {
      const raw = await readJson(join(this.dir, 'index.json'))
      if (raw !== undefined) {
        for (const chat of indexSchema.parse(raw).chats) this.chats.set(chat.id, { ...chat, workspaceId: chat.workspaceId === null ? null : brandString<WorkspaceId>(chat.workspaceId) })
        return
      }
    } catch (error) {
      console.error(`[chats] index unreadable, rebuilding: ${error instanceof Error ? error.message : String(error)}`)
    }
    await this.rebuildIndex()
  }

  /** Scan every chat file and rewrite the index. */
  async rebuildIndex(): Promise<void> {
    this.chats.clear()
    for (const name of await readdir(this.dir)) {
      if (!name.endsWith('.jsonl')) continue
      try {
        const file = await this.readFile(name.slice(0, -'.jsonl'.length), true)
        if (file !== undefined) this.chats.set(file.chat.id, file.chat)
      } catch (error) {
        console.error(`[chats] skipping ${name}: ${error instanceof Error ? error.message : String(error)}`)
      }
    }
    await this.writeIndex()
  }

  /** @returns Every chat, newest activity first. */
  list(): StoredChat[] {
    return [...this.chats.values()].sort((left, right) => right.updatedAt - left.updatedAt)
  }

  /** @param id Chat id. @returns Its summary. */
  get(id: SessionId): StoredChat | undefined {
    return this.chats.get(id)
  }

  /**
   * Create an empty chat.
   * @param options Project, working directory and fork origin.
   * @returns The new chat.
   */
  async create(options: { workspaceId: WorkspaceId | null; cwd: string; parent?: { sessionId: SessionId; entryId: string } | undefined; entries?: TranscriptEntry[] | undefined; title?: string | undefined; model?: ModelSelection | null | undefined }): Promise<StoredChat> {
    const now = Date.now()
    const chat: StoredChat = {
      id: randomUUID(), workspaceId: options.workspaceId, cwd: options.cwd, title: options.title ?? '', createdAt: now, updatedAt: now,
      archived: false, pinned: false, model: options.model ?? null, ...(options.parent === undefined ? {} : { parent: options.parent }),
    }
    const lines = [
      JSON.stringify({ type: 'header', version: CHAT_FORMAT_VERSION, id: chat.id, workspaceId: chat.workspaceId, cwd: chat.cwd, createdAt: now, ...(chat.parent === undefined ? {} : { parent: chat.parent }) }),
      ...(options.entries ?? []).map(entry => JSON.stringify({ type: 'entry', entry })),
      ...(chat.title !== '' || chat.model !== null ? [JSON.stringify({ type: 'meta', ...(chat.title === '' ? {} : { title: chat.title }), ...(chat.model === null ? {} : { model: chat.model }) })] : []),
    ]
    await writeFileAtomic(this.path(chat.id), `${lines.join('\n')}\n`)
    this.chats.set(chat.id, chat)
    this.scheduleIndex()
    return chat
  }

  /**
   * Read a chat's entries.
   * @param id Chat id.
   * @returns The chat and its entries in order.
   */
  async read(id: SessionId): Promise<ChatFile> {
    const file = await this.queue(id).run(() => this.readFile(id, false))
    if (file === undefined) throw new Error(`Chat ${id} not found`)
    return file
  }

  /**
   * Append entries.
   * @param id Chat id.
   * @param entries Entries in order.
   */
  async append(id: SessionId, entries: readonly TranscriptEntry[]): Promise<void> {
    if (entries.length === 0) return
    const chat = this.require(id)
    await this.queue(id).run(() => appendFile(this.path(id), entries.map(entry => `${JSON.stringify({ type: 'entry', entry })}\n`).join(''), { mode: 0o600 }))
    chat.updatedAt = Date.now()
    this.scheduleIndex()
  }

  /**
   * Record metadata changes.
   * @param id Chat id.
   * @param patch Fields to change.
   * @returns The updated summary.
   */
  async setMeta(id: SessionId, patch: ChatMetaPatch): Promise<StoredChat> {
    const chat = this.require(id)
    await this.queue(id).run(() => appendFile(this.path(id), `${JSON.stringify({ type: 'meta', ...patch })}\n`, { mode: 0o600 }))
    Object.assign(chat, patch)
    this.scheduleIndex()
    return chat
  }

  /**
   * Delete a chat file.
   * @param id Chat id.
   */
  async delete(id: SessionId): Promise<void> {
    await this.queue(id).run(() => rm(this.path(id), { force: true }))
    this.chats.delete(id)
    this.queues.delete(id)
    this.scheduleIndex()
  }

  /**
   * Find chats whose title or text contains `query` (case-insensitive).
   * @param query Search text.
   * @param limit Maximum hits.
   * @returns Matching chats with a short snippet.
   */
  async search(query: string, limit: number): Promise<{ chat: StoredChat; snippet: string }[]> {
    const needle = query.trim().toLowerCase()
    if (needle === '') return []
    const hits: { chat: StoredChat; snippet: string }[] = []
    for (const chat of this.list()) {
      if (hits.length >= limit) break
      if (chat.title.toLowerCase().includes(needle)) { hits.push({ chat, snippet: chat.title }); continue }
      const snippet = await this.findText(chat.id, needle)
      if (snippet !== undefined) hits.push({ chat, snippet })
    }
    return hits
  }

  /** Write any pending index change now. */
  async flush(): Promise<void> {
    if (this.indexTimer !== undefined) { clearTimeout(this.indexTimer); this.indexTimer = undefined }
    await this.writeIndex()
  }

  private async findText(id: SessionId, needle: string): Promise<string | undefined> {
    const lines = createInterface({ input: createReadStream(this.path(id), { encoding: 'utf8' }), crlfDelay: Infinity })
    try {
      for await (const line of lines) {
        if (!line.includes('"type":"entry"')) continue
        const parsed: unknown = JSON.parse(line)
        const entry = (parsed as { entry?: TranscriptEntry }).entry
        const text = entry === undefined ? '' : entryText(entry)
        const index = text.toLowerCase().indexOf(needle)
        if (index >= 0) return text.slice(Math.max(0, index - 40), index + needle.length + 80).replace(/\s+/g, ' ')
      }
    } catch (error) {
      console.error(`[chats] search skipped ${id}: ${error instanceof Error ? error.message : String(error)}`)
    } finally {
      lines.close()
    }
    return undefined
  }

  private async readFile(id: string, headerOnly: boolean): Promise<ChatFile | undefined> {
    if (!ID_PATTERN.test(id)) return undefined
    let text: string
    try { text = await readFile(this.path(id), 'utf8') } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
      throw error
    }
    const lines = text.split('\n')
    const header = headerSchema.parse(JSON.parse(lines[0] ?? ''))
    const info = await stat(this.path(id))
    const chat: StoredChat = {
      id: header.id, workspaceId: header.workspaceId === null ? null : brandString<WorkspaceId>(header.workspaceId), cwd: header.cwd, title: '',
      createdAt: header.createdAt, updatedAt: info.mtimeMs, archived: false, pinned: false, model: null,
      ...(header.parent === undefined ? {} : { parent: header.parent }),
    }
    const entries: TranscriptEntry[] = []
    for (let index = 1; index < lines.length; index++) {
      const line = lines[index]!
      if (line === '') continue
      let record: unknown
      // A crash can leave a partial last line; everything before it is intact.
      try { record = JSON.parse(line) } catch (_error) { if (index === lines.length - 1 || lines[index + 1] === '') continue; throw new Error(`Corrupt line ${index + 1} in chat ${id}`) }
      if (record === null || typeof record !== 'object') continue
      const type = (record as { type?: unknown }).type
      if (type === 'meta') {
        const meta = metaSchema.parse(record)
        if (meta.title !== undefined) chat.title = meta.title
        if (meta.archived !== undefined) chat.archived = meta.archived
        if (meta.pinned !== undefined) chat.pinned = meta.pinned
        if (meta.model !== undefined) chat.model = meta.model
      } else if (type === 'entry' && !headerOnly) entries.push((record as { entry: TranscriptEntry }).entry)
    }
    return { chat, entries }
  }

  private require(id: SessionId): StoredChat {
    const chat = this.chats.get(id)
    if (chat === undefined) throw new Error(`Chat ${id} not found`)
    return chat
  }

  private path(id: string): string {
    return join(this.dir, `${id}.jsonl`)
  }

  private queue(id: SessionId): SerialQueue {
    let queue = this.queues.get(id)
    if (queue === undefined) { queue = new SerialQueue(); this.queues.set(id, queue) }
    return queue
  }

  private scheduleIndex(): void {
    if (this.indexTimer !== undefined) return
    this.indexTimer = setTimeout(() => {
      this.indexTimer = undefined
      void this.writeIndex().catch((error: unknown) => { console.error(`[chats] index write failed: ${String(error)}`) })
    }, 500)
  }

  private writeIndex(): Promise<void> {
    return this.indexQueue.run(() => writeJson(join(this.dir, 'index.json'), { version: 1, chats: [...this.chats.values()] }))
  }
}

/**
 * Searchable text of an entry.
 * @param entry Transcript entry.
 * @returns User text, assistant text, or tool output text.
 */
export function entryText(entry: TranscriptEntry): string {
  switch (entry.kind) {
    case 'user': return entry.text
    case 'assistant': return entry.message.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')
    case 'toolResult': return entry.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')
    case 'context': return entry.text
    case 'compaction': return entry.summary
    case 'notice': return entry.text
    case 'turn': return ''
  }
}
