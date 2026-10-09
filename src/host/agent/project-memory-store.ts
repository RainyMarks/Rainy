/** Versioned project notes shared by the Windows and WSL hosts of one desktop. */
import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { createHash, randomUUID } from 'node:crypto'
import { z } from 'zod'
import { estimateText } from './budget.ts'

/** Persisted note length limit, shared by the record schema and the human edit message. */
const MAX_NOTE_LENGTH = 2048
const identifier = z.string().regex(/^[a-zA-Z0-9_.:-]{1,160}$/)
const sourceSchema = z.object({
  formatVersion: z.number().int().nonnegative().default(4),
  sessionId: identifier,
  seq: z.number().int().nonnegative(),
  executionTargetId: identifier,
  file: z.object({ path: z.string().min(1).max(32768), version: z.string().min(1).max(256) }).optional(),
})
const itemSchema = z.object({
  id: z.uuid(),
  text: z.string().min(1).max(MAX_NOTE_LENGTH),
  sources: z.array(sourceSchema).min(1).max(8),
  updatedAt: z.iso.datetime(),
  editedByUser: z.literal(true).optional(),
  scope: z.enum(['project', 'execution-target']).default('project'),
})
const documentSchema = z.object({
  version: z.literal(1),
  revision: z.number().int().nonnegative(),
  enabled: z.boolean(),
  generationEnabled: z.boolean().default(true),
  updatedAt: z.iso.datetime().nullable(),
  items: z.array(itemSchema).max(32),
  lastAttemptAt: z.number().int().nonnegative().default(0),
  excluded: z.array(z.string().regex(/^[a-f0-9]{64}$/)).max(1024),
  sourceWatermarks: z.record(z.string(), z.number().int().nonnegative()),
})

/** Durable origin of one generated fact, retaining the execution environment. */
export type MemorySource = z.infer<typeof sourceSchema>
/** A generated historical fact; its text never carries instruction authority. */
export type ProjectMemoryItem = z.infer<typeof itemSchema>
/** Validated on-disk record. Source watermarks prevent deleted notes from reappearing. */
export type ProjectMemoryDocument = z.infer<typeof documentSchema>
/** One source fragment made available to the summarizer. */
export interface MemoryEvidence extends MemorySource {
  id: string
  kind: 'user' | 'assistant' | 'tool'
  text: string
}
/** Small validated changes returned by a tool-free memory generation. */
export const MemoryDelta = z
  .object({
    notes: z
      .array(
        z
          .object({
            text: z.string().trim().min(1).max(1024),
            sourceIds: z.array(z.string()).min(1).max(8),
            replaceId: z.uuid().optional(),
            scope: z.enum(['project', 'execution-target']).optional(),
          })
          .strict(),
      )
      .max(8),
    remove: z.array(z.uuid()).max(8),
  })
  .strict()

const fresh = (): ProjectMemoryDocument => ({
  version: 1,
  revision: 0,
  enabled: true,
  generationEnabled: true,
  updatedAt: null,
  lastAttemptAt: 0,
  items: [],
  excluded: [],
  sourceWatermarks: {},
})
const digest = (text: string): string =>
  createHash('sha256').update(text.trim().replace(/\s+/g, ' ').toLowerCase()).digest('hex')

/**
 * Omit common credential material before model input or generated-note persistence.
 * @param text Source or generated text to project into project memory.
 * @returns Text with credential values replaced by a marker.
 */
export function redactMemoryText(text: string): string {
  return text
    .replace(/-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, '[private key omitted]')
    .replace(/\b(?:sk|ghp|gho|github_pat|xox[baprs])[-_][A-Za-z0-9_-]{16,}\b/g, '[credential omitted]')
    .replace(
      /\b(api[_ -]?key|access[_ -]?token|password|secret|authorization)\s*[:=]\s*["']?[^\s,"'}]+/gi,
      '$1=[credential omitted]',
    )
    .replace(/\bBearer\s+[a-z0-9._~+/-]+=*/gi, 'Bearer [credential omitted]')
}

/**
 * Admit only notes grounded in the exact source fragments of this generation.
 * @param current Last committed project record.
 * @param raw Untrusted JSON from the summarizer.
 * @param evidence Exact bounded source fragments supplied to that call.
 * @param maxTokens Maximum estimated tokens retained across all active note text.
 * @param now Commit timestamp.
 * @returns A candidate record; publication still requires a revision check.
 */
export function applyMemoryDelta(
  current: ProjectMemoryDocument,
  raw: unknown,
  evidence: readonly MemoryEvidence[],
  maxTokens: number,
  now: string,
): ProjectMemoryDocument {
  const delta = MemoryDelta.parse(raw)
  const available = new Map(evidence.map(item => [item.id, item]))
  const existing = new Map(current.items.map(item => [item.id, item]))
  for (const id of delta.remove) {
    if (!existing.has(id)) throw new Error('Memory generation refers to an unknown existing note.')
    if (existing.get(id)?.editedByUser) throw new Error('Generated memory cannot remove a manually edited note.')
    existing.delete(id)
  }
  for (const proposed of delta.notes) {
    const text = redactMemoryText(proposed.text)
    if (current.excluded.includes(digest(text))) continue
    if (!proposed.sourceIds.some(id => available.get(id)?.kind === 'user' || available.get(id)?.kind === 'tool'))
      throw new Error('Assistant claims alone cannot become verified project memory.')
    const sources = proposed.sourceIds.map((id): MemorySource => {
      const source = available.get(id)
      if (!source) throw new Error('Memory generation refers to evidence it did not receive.')
      return {
        formatVersion: source.formatVersion,
        sessionId: source.sessionId,
        seq: source.seq,
        executionTargetId: source.executionTargetId,
        ...(source.file === undefined ? {} : { file: source.file }),
      }
    })
    if (proposed.replaceId !== undefined && !current.items.some(item => item.id === proposed.replaceId)) {
      throw new Error('Memory generation replaces an unknown note.')
    }
    if (proposed.replaceId !== undefined && existing.get(proposed.replaceId)?.editedByUser)
      throw new Error('Generated memory cannot overwrite a manually edited note.')
    if (proposed.replaceId !== undefined) existing.delete(proposed.replaceId)
    const duplicate = [...existing.values()].find(item => digest(item.text) === digest(text))
    if (duplicate?.editedByUser) continue
    const scope =
      proposed.scope ??
      (proposed.sourceIds.some(id => available.get(id)?.kind === 'tool') ? 'execution-target' : 'project')
    const item: ProjectMemoryItem = {
      id: duplicate?.id ?? proposed.replaceId ?? randomUUID(),
      text,
      sources,
      updatedAt: now,
      scope,
    }
    existing.set(item.id, item)
  }
  const items: ProjectMemoryItem[] = []
  let tokens = 0
  for (const item of [...existing.values()].sort(
    (a, b) =>
      Number(!!b.editedByUser) - Number(!!a.editedByUser) ||
      b.updatedAt.localeCompare(a.updatedAt) ||
      a.id.localeCompare(b.id),
  )) {
    const size = estimateText(item.text)
    if (tokens + size > maxTokens || items.length === 32) continue
    items.push(item)
    tokens += size
  }
  const sourceWatermarks = { ...current.sourceWatermarks }
  for (const source of evidence) {
    const key = `${source.executionTargetId}/${source.sessionId}`
    sourceWatermarks[key] = Math.max(sourceWatermarks[key] ?? -1, source.seq)
  }
  return { ...current, items, sourceWatermarks, revision: current.revision + 1, updatedAt: now }
}

/**
 * One serialized writer per store; the desktop permits only one active Host.
 * Invalid records fail explicitly and are never overwritten with empty state.
 */
export class ProjectMemoryStore {
  private tail: Promise<unknown> = Promise.resolve()
  /** @param root Central carrier's project-memory directory, mapped into the current execution target. */
  constructor(private readonly root: string) {}

  /** @param projectId Stable carrier project identity. @returns Absolute path accepted by the ordinary file reader. */
  path(projectId: string): string {
    if (!/^[a-zA-Z0-9_.-]{1,160}$/.test(projectId) || projectId === '.' || projectId === '..')
      throw new Error('Invalid project identity.')
    return join(this.root, projectId, 'memory.v1.json')
  }

  /** @param projectId Stable carrier project identity. @returns Validated current notes, or an empty enabled record for a new project. */
  async read(projectId: string): Promise<ProjectMemoryDocument> {
    await this.tail
    return this.readCurrent(projectId)
  }

  private async readCurrent(projectId: string): Promise<ProjectMemoryDocument> {
    try {
      return documentSchema.parse(JSON.parse(await readFile(this.path(projectId), 'utf8')))
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return fresh()
      throw error
    }
  }

  /**
   * Serialize a human edit or generation commit with atomic file publication.
   * @param projectId Stable carrier identity.
   * @param edit Transformation of the latest validated record; undefined leaves it unchanged.
   * @returns The resulting committed record.
   */
  update(
    projectId: string,
    edit: (current: ProjectMemoryDocument) => ProjectMemoryDocument | undefined,
  ): Promise<ProjectMemoryDocument> {
    const task = this.tail.then(async () => {
      const current = await this.readCurrent(projectId)
      const next = edit(current)
      if (!next) return current
      documentSchema.parse(next)
      const path = this.path(projectId)
      await mkdir(join(this.root, projectId), { recursive: true, mode: 0o700 })
      const temporary = path + '.' + randomUUID() + '.tmp'
      try {
        await writeFile(temporary, JSON.stringify(next) + '\n', { encoding: 'utf8', flag: 'wx', mode: 0o600 })
        await rename(temporary, path)
      } catch (error) {
        await rm(temporary, { force: true }).catch((_cleanupError: unknown) => { /* Preserve the original write failure. */ })
        throw error
      }
      return next
    })
    this.tail = task.catch(() => undefined)
    return task
  }

  /**
   * Remove one note or clear all notes while retaining source watermarks and deletion exclusions.
   * @param projectId Stable project identity.
   * @param id Note identity, or omitted to clear the active set.
   * @returns Committed state after the human deletion.
   */
  remove(projectId: string, id?: string): Promise<ProjectMemoryDocument> {
    return this.update(projectId, current => ({
      ...current,
      revision: current.revision + 1,
      updatedAt: new Date().toISOString(),
      excluded: [
        ...new Set([
          ...current.excluded,
          ...current.items.filter(item => id === undefined || item.id === id).map(item => digest(item.text)),
        ]),
      ].slice(-1024),
      items: current.items.filter(item => id !== undefined && item.id !== id),
    }))
  }

  /**
   * Save a human correction without allowing a background generation to overwrite it.
   * @param projectId Stable project identity.
   * @param edit Note, expected record revision, and replacement text.
   * @param maxTokens Complete active-note text budget.
   * @returns The atomically committed correction.
   */
  edit(
    projectId: string,
    edit: { id: string; text: string; expectedRevision: number },
    maxTokens: number,
  ): Promise<ProjectMemoryDocument> {
    return this.update(projectId, (current) => {
      if (current.revision !== edit.expectedRevision) throw new Error('项目记忆已更新，请刷新后重试。')
      const old = current.items.find(item => item.id === edit.id)
      if (!old) throw new Error('项目记忆条目不存在。')
      const updatedAt = new Date().toISOString()
      const text = redactMemoryText(edit.text.trim())
      if (!text) throw new Error('项目记忆不能为空。')
      if (text.length > MAX_NOTE_LENGTH) throw new Error(`单条项目记忆不能超过 ${MAX_NOTE_LENGTH} 个字符。`)
      const items = current.items.map(item =>
        item.id === edit.id ? { ...item, text, updatedAt, editedByUser: true as const } : item,
      )
      if (items.reduce((sum, item) => sum + estimateText(item.text), 0) > maxTokens)
        throw new Error('项目记忆超过总 token 预算，请精简内容。')
      return {
        ...current,
        revision: current.revision + 1,
        updatedAt,
        items,
        excluded: [...new Set([...current.excluded, digest(old.text)])].slice(-1024),
      }
    })
  }
}
