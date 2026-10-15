/** What a file held before the agent's write or edit changed it, kept so the user can undo the change. */
import { createHash } from 'node:crypto'
import { mkdir, readdir, readFile, rm, stat, unlink, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { z } from 'zod'

/** Files larger than this keep no snapshot, so their changes cannot be undone. */
export const MAX_SNAPSHOT_BYTES = 16 * 1024 * 1024

const recordSchema = z.object({
  version: z.literal(1),
  path: z.string(),
  /** Hash of the content before the change; `null` when the change created the file. */
  before: z.string().nullable(),
  /** Hash of the content the change wrote. */
  after: z.string(),
})

/** Why a change could not be undone. */
export interface RevertFailure { reason: 'changed' | 'unavailable' | 'failed'; message: string; path: string }

function digest(bytes: Uint8Array): string {
  return createHash('sha256').update(bytes).digest('hex')
}

/** Tool call ids come from providers; their hash makes a safe file name. */
function fileStem(toolCallId: string): string {
  return digest(Buffer.from(toolCallId, 'utf8')).slice(0, 40)
}

/** Snapshots under `<home>/snapshots/<sessionId>/`, one record and one content file per change. */
export class ChangeSnapshots {
  /** @param root `<home>/snapshots`. */
  constructor(private readonly root: string) {}

  /**
   * Record a change after it was written.
   * @param sessionId Chat id.
   * @param toolCallId The write or edit call.
   * @param path Absolute file path.
   * @param before Content before the change, or `null` when the file did not exist.
   * @param after Content the change wrote.
   */
  async record(sessionId: string, toolCallId: string, path: string, before: Uint8Array | null, after: Uint8Array): Promise<void> {
    const directory = join(this.root, sessionId)
    await mkdir(directory, { recursive: true, mode: 0o700 })
    const stem = fileStem(toolCallId)
    if (before !== null) await writeFile(join(directory, `${stem}.bin`), before, { mode: 0o600 })
    const record: z.infer<typeof recordSchema> = { version: 1, path, before: before === null ? null : digest(before), after: digest(after) }
    await writeFile(join(directory, `${stem}.json`), JSON.stringify(record), { mode: 0o600 })
  }

  /**
   * What a file held before one change.
   * @param sessionIds The chat and the chats it was forked from, nearest first.
   * @param toolCallId The write or edit call.
   * @returns The bytes, `null` when the change created the file, or `undefined` when they are not kept.
   */
  async original(sessionIds: readonly string[], toolCallId: string): Promise<Buffer | null | undefined> {
    const found = await this.find(sessionIds, toolCallId)
    if (found === undefined) return undefined
    if (found.record.before === null) return null
    const bytes = await readFile(join(found.directory, `${fileStem(toolCallId)}.bin`)).catch(() => undefined)
    return bytes !== undefined && digest(bytes) === found.record.before ? bytes : undefined
  }

  private async find(sessionIds: readonly string[], toolCallId: string): Promise<{ directory: string; record: z.infer<typeof recordSchema> } | undefined> {
    const stem = fileStem(toolCallId)
    for (const sessionId of sessionIds) {
      const directory = join(this.root, sessionId)
      const text = await readFile(join(directory, `${stem}.json`), 'utf8').catch(() => undefined)
      if (text === undefined) continue
      let raw: unknown
      try { raw = JSON.parse(text) } catch (_damaged) { continue }
      const parsed = recordSchema.safeParse(raw)
      if (parsed.success) return { directory, record: parsed.data }
    }
    return undefined
  }

  /**
   * Put a file back to its content before one change, if it still holds what that change wrote.
   * @param sessionIds The chat and the chats it was forked from, nearest first; a fork can undo its parent's changes.
   * @param toolCallId The write or edit call.
   * @returns The restored path, or why nothing was restored.
   */
  async revert(sessionIds: readonly string[], toolCallId: string): Promise<{ path: string } | RevertFailure> {
    const stem = fileStem(toolCallId)
    const found = await this.find(sessionIds, toolCallId)
    if (found === undefined) return { reason: 'unavailable', path: '', message: 'The original content is no longer kept.' }
    const { directory, record } = found
    let current: Buffer
    try { current = await readFile(record.path) } catch (_missing) {
      return { reason: 'changed', path: record.path, message: 'The file was moved or deleted after the change.' }
    }
    if (digest(current) !== record.after) return { reason: 'changed', path: record.path, message: 'The file changed after the change was made.' }
    try {
      if (record.before === null) await unlink(record.path)
      else {
        const original = await readFile(join(directory, `${stem}.bin`))
        if (digest(original) !== record.before) return { reason: 'unavailable', path: record.path, message: 'The saved original content is damaged.' }
        await writeFile(record.path, original)
      }
    } catch (error) {
      return { reason: 'failed', path: record.path, message: error instanceof Error ? error.message : String(error) }
    }
    await rm(join(directory, `${stem}.json`), { force: true })
    await rm(join(directory, `${stem}.bin`), { force: true })
    return { path: record.path }
  }

  /** @param sessionId Chat whose snapshots go away with it. */
  async remove(sessionId: string): Promise<void> {
    await rm(join(this.root, sessionId), { recursive: true, force: true })
  }

  /**
   * Delete snapshots older than the retention period.
   * @param retentionMs Age limit.
   */
  async sweep(retentionMs: number): Promise<void> {
    const cutoff = Date.now() - retentionMs
    for (const session of await readdir(this.root).catch(() => [] as string[])) {
      const directory = join(this.root, session)
      for (const name of await readdir(directory).catch(() => [] as string[])) {
        const path = join(directory, name)
        const info = await stat(path).catch(() => undefined)
        if (info !== undefined && info.mtimeMs < cutoff) await rm(path, { force: true })
      }
    }
  }
}
