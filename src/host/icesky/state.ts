/** Revision-checked IceSky workbench drafts in `<home>/icesky/state.json`; credentials are never stored. */
import type { IncomingMessage, ServerResponse } from 'node:http'
import { z } from 'zod'
import { brandString, type Branded } from '../../shared/brand.ts'
import { readJson, writeJson } from '../files.ts'
import { readBody } from '../server.ts'

/** Standalone (`standalone`) or chat-scoped (`session:<id>`) draft identity. */
export type IceSkyDraftScope = Branded<'IceSkyDraftScope'>

const credentialNames = new RegExp('^(?:apikey|openaiapikey|anthropicapikey|openrouterapikey|openaikey|anthropickey|'
  + 'authorization|accesstoken|refreshtoken|credentials|credential|secret|clientsecret|password)$')
function hasCredentialField(value: unknown): boolean {
  const pending: unknown[] = [value]
  while (pending.length) {
    const next = pending.pop()
    if (next === null || typeof next !== 'object') continue
    for (const [key, child] of Object.entries(next)) {
      if (credentialNames.test(key.replace(/[^a-z0-9]/gi, '').toLowerCase())) return true
      pending.push(child)
    }
  }
  return false
}
const draftJson = z.json()
const fileMetadata = z.object({
  name: z.string().optional(), size: z.number().nonnegative().optional(),
  type: z.string().optional(), lastModified: z.number().nonnegative().optional(),
}).strict()
const draftData = z.object({
  shared: z.record(z.string(), draftJson).optional(),
  tools: z.record(z.string(), z.object({
    fields: z.record(z.string(), draftJson).optional(), files: z.record(z.string(), fileMetadata).optional(),
  }).strict()).optional(),
  legacyMigrated: z.boolean().optional(),
}).strict().refine(value => !hasCredentialField(value), 'API credentials cannot be saved in a workbench draft.')
const recordSchema = z.object({
  version: z.literal(1), revision: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER), data: draftData,
}).strict()
const fileSchema = z.object({ version: z.literal(1), drafts: z.record(z.string(), recordSchema) }).strict()
const writeSchema = z.object({
  baseRevision: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER - 1), data: draftData,
}).strict()
const clearSchema = writeSchema.omit({ data: true }).transform(command => ({ ...command, data: {} }))

/** A complete committed draft; an absent record reads as revision zero with empty data. */
export type IceSkyDraftEnvelope = z.infer<typeof recordSchema>

/** A stale save or clear request with the authoritative committed value attached. */
export class IceSkyDraftConflict extends Error {
  /** @param current Value that superseded the caller's base revision. */
  constructor(readonly current: IceSkyDraftEnvelope) {
    super('The workbench draft changed before this request was saved.')
  }
}

/** Serializes compare-and-save operations before each atomic file write. */
export class IceSkyDraftStore {
  private chain: Promise<void> = Promise.resolve()
  private closing = false
  private disposal?: Promise<void>

  private constructor(
    private readonly path: string,
    private drafts: Record<string, IceSkyDraftEnvelope>,
    private readonly write: (path: string, value: unknown) => Promise<void>,
  ) {}

  /**
   * Load committed drafts.
   * @param path `state.json` location; a missing file starts empty.
   * @param write Atomic JSON publication; defaults to {@link writeJson}.
   * @returns The store.
   * @throws Error when the file exists but is not a supported draft file; it is left unchanged.
   */
  static async open(path: string, write: (path: string, value: unknown) => Promise<void> = writeJson): Promise<IceSkyDraftStore> {
    const raw = await readJson(path)
    return new IceSkyDraftStore(path, raw === undefined ? {} : fileSchema.parse(raw).drafts, write)
  }

  /**
   * @param scope Admitted identity.
   * @returns The last committed value.
   */
  get(scope: IceSkyDraftScope): IceSkyDraftEnvelope {
    return structuredClone(this.drafts[scope] ?? { version: 1, revision: 0, data: {} })
  }

  /**
   * Save a complete draft or an empty tombstone, comparing at the serialized write slot.
   * @param scope Admitted identity.
   * @param baseRevision Revision the caller read.
   * @param data Parsed text and parameters, credentials excluded.
   * @returns The committed value after the atomic write.
   */
  replace(scope: IceSkyDraftScope, baseRevision: number, data: IceSkyDraftEnvelope['data']): Promise<IceSkyDraftEnvelope> {
    if (this.closing) return Promise.reject(new Error('Workbench draft storage is closing.'))
    const result = this.chain.then(async () => {
      const current = this.get(scope)
      if (current.revision !== baseRevision) throw new IceSkyDraftConflict(current)
      const next: IceSkyDraftEnvelope = { version: 1, revision: current.revision + 1, data: structuredClone(data) }
      const drafts = { ...this.drafts, [scope]: next }
      await this.write(this.path, { version: 1, drafts })
      this.drafts = drafts
      return structuredClone(next)
    })
    this.chain = result.then(() => undefined, () => undefined)
    return result
  }

  /** @returns Completion after pending saves drain; later saves are rejected. */
  close(): Promise<void> {
    this.closing = true
    this.disposal ??= this.chain
    return this.disposal
  }
}

/** Dependencies of {@link createIceSkyDraftHandler}. */
export interface IceSkyDraftRouteOptions {
  /** Whether a chat exists; consulted only for `session:` scopes. */
  readonly sessionExists: (sessionId: string, signal: AbortSignal) => Promise<boolean>
  /** Largest complete JSON request. */
  readonly maxDraftBytes: number
}

class RequestFailure extends Error {
  constructor(readonly status: number, readonly code: string, message: string) { super(message) }
}

async function scopeOf(url: URL, options: IceSkyDraftRouteOptions, signal: AbortSignal): Promise<IceSkyDraftScope> {
  const scopes = url.searchParams.getAll('scope')
  if (scopes.length !== 1) throw new RequestFailure(400, 'invalid-scope', '请选择独立草稿或一个聊天。')
  const scope = scopes[0] ?? ''
  if (scope === 'standalone') return brandString<IceSkyDraftScope>(scope)
  if (!scope.startsWith('session:')) throw new RequestFailure(400, 'invalid-scope', '草稿所属聊天无效。')
  const raw = scope.slice('session:'.length)
  if (!raw || raw.length > 512 || /[\x00-\x20/\\]/.test(raw)) throw new RequestFailure(400, 'invalid-scope', '草稿所属聊天无效。')
  if (!await options.sessionExists(raw, signal)) throw new RequestFailure(404, 'session-not-found', '所属聊天不存在。')
  return brandString<IceSkyDraftScope>(scope)
}

async function readRequest(request: IncomingMessage, maxBytes: number): Promise<unknown> {
  const tooLarge = new RequestFailure(413, 'draft-too-large', '草稿超过保存大小限制。')
  const declared = request.headers['content-length']
  if (declared !== undefined && (!/^\d+$/u.test(declared) || Number(declared) > maxBytes)) throw tooLarge
  let body: Buffer
  try { body = await readBody(request, maxBytes) } catch (_overLimit) { throw tooLarge }
  try { return JSON.parse(body.toString('utf8')) }
  catch (_invalidDraft) { throw new RequestFailure(400, 'invalid-draft', '草稿 JSON 无效。') }
}

function json(response: ServerResponse, status: number, value: object): void {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' })
  response.end(JSON.stringify(value))
}

/**
 * Create the draft route.
 * @param store Draft store, or `undefined` when its file could not be read (every request then answers 503).
 * @param options Chat lookup and request budget.
 * @returns The handler for GET, PUT and DELETE on `/rainy/icesky/state`; the server calls it only for authenticated requests.
 */
export function createIceSkyDraftHandler(
  store: IceSkyDraftStore | undefined, options: IceSkyDraftRouteOptions,
): (request: IncomingMessage, response: ServerResponse, url: URL) => Promise<void> {
  return async (request, response, url) => {
    if (!['GET', 'PUT', 'DELETE'].includes(request.method ?? '')) { response.writeHead(405, { Allow: 'GET, PUT, DELETE' }); response.end(); return }
    const controller = new AbortController()
    const disconnected = (): void => { controller.abort() }
    response.once('close', disconnected)
    try {
      const scope = await scopeOf(url, options, controller.signal)
      if (store === undefined) throw new Error('Workbench draft storage is unavailable.')
      if (request.method === 'GET') { json(response, 200, store.get(scope)); return }
      const body = await readRequest(request, options.maxDraftBytes)
      const parsed = request.method === 'PUT' ? writeSchema.safeParse(body) : clearSchema.safeParse(body)
      if (!parsed.success) throw new RequestFailure(400, 'invalid-draft', '草稿只能保存文字和参数，不能包含 API 密钥。')
      json(response, 200, await store.replace(scope, parsed.data.baseRevision, parsed.data.data))
    } catch (error) {
      if (response.destroyed) return
      if (error instanceof IceSkyDraftConflict) {
        json(response, 409, { ...error.current, error: { code: 'revision-conflict', message: '草稿已被更新，请先重新读取。' } })
      } else if (error instanceof RequestFailure) {
        json(response, error.status, { error: { code: error.code, message: error.message } })
      } else {
        json(response, 503, { error: { code: 'draft-storage-unavailable', message: '草稿保存失败，当前输入仍可继续使用。' } })
      }
    } finally { response.removeListener('close', disconnected) }
  }
}
