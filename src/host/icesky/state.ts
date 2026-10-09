/** Durable, revision-checked human workbench drafts; credentials stay outside this domain. */
import type { IncomingMessage, ServerResponse } from 'node:http'
import { readRequestBytes } from './request-body.ts'
import type { Context } from '@deepseek-ai/cordis'
import s from '@deepseek-ai/schemastery'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { Branded } from '@deepseek-ai/dsh-brand'
import { defineDomain, domainTable } from '@deepseek-ai/dsh-storage-domain'
import type { Domain } from '@deepseek-ai/dsh-storage-domain'
import { SessionId } from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-session-persistence'
import type {} from '@deepseek-ai/dsh-agent'
import type {} from '@deepseek-ai/dsh-host-webserver'
import type {} from '@deepseek-ai/dsh-client-connection'
import { z } from 'zod'

/** Authenticated standalone or Session-scoped draft identity. */
export type IceSkyDraftScope = Branded<'IceSkyDraftScope'>

const credentialNames = new RegExp('^(?:apikey|openaiapikey|anthropicapikey|openrouterapikey|openaikey|anthropickey|'
  + 'authorization|accesstoken|refreshtoken|credentials|credential|secret|clientsecret|password)$')
function hasCredentialField(value: unknown): boolean {
  const pending: unknown[] = [value]
  while (pending.length) {
    const next = pending.pop()
    if (next === null || typeof next !== 'object') continue
    for (const [key, child] of Object.entries(next)) {
      if (credentialNames.test(key.replace(/[^a-z0-9]/gi, '').toLowerCase())) {
        return true
      }
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
const writeSchema = z.object({
  baseRevision: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER - 1), data: draftData,
}).strict()
const clearSchema = writeSchema.omit({ data: true }).transform(command => ({ ...command, data: {} }))

/** A complete committed draft; an absent record reads as revision zero with empty data. */
export type IceSkyDraftEnvelope = z.infer<typeof recordSchema>

/** Persistent format owned by this feature, separate from released Session logs. */
export const iceSkyDraftSpec = defineDomain({
  name: 'rainy_icesky', version: 1, layout: 'single',
  tables: { drafts: domainTable<IceSkyDraftScope, IceSkyDraftEnvelope>(recordSchema) },
})

/** A stale save or clear request with the authoritative committed value attached. */
export class IceSkyDraftConflict extends Error {
  /** @param current - value that superseded the caller's base revision. */
  constructor(readonly current: IceSkyDraftEnvelope) {
    super('The workbench draft changed before this request was saved.')
  }
}

/** Serializes compare-and-save operations before the domain's atomic durable publication. */
export class IceSkyDraftStore {
  private chain: Promise<void> = Promise.resolve()
  private closing = false
  private disposal?: Promise<void>

  /** @param domain - exclusively owned draft domain. */
  constructor(private readonly domain: Domain<typeof iceSkyDraftSpec>) {}

  /**
   * @param scope - admitted identity.
   * @returns the last durably committed value.
   */
  get(scope: IceSkyDraftScope): IceSkyDraftEnvelope {
    return this.domain.table('drafts').get(scope) ?? { version: 1, revision: 0, data: {} }
  }

  /**
   * Save a complete draft or an empty tombstone, comparing at the serialized write slot.
   * @param scope - admitted identity.
   * @param baseRevision - revision the caller read.
   * @param data - parsed JSON containing text and parameters, with credentials excluded.
   * @returns the committed value after the atomic backend write resolves.
   */
  replace(scope: IceSkyDraftScope, baseRevision: number, data: IceSkyDraftEnvelope['data']): Promise<IceSkyDraftEnvelope> {
    if (this.closing) return Promise.reject(new Error('Workbench draft storage is closing.'))
    const result = this.chain.then(async () => {
      const current = this.get(scope)
      if (current.revision !== baseRevision) throw new IceSkyDraftConflict(current)
      const next: IceSkyDraftEnvelope = { version: 1, revision: current.revision + 1, data }
      await this.domain.table('drafts').put(scope, next)
      return next
    })
    this.chain = result.then(() => undefined, () => undefined)
    return result
  }

  /** @returns resolution after pending saves drain and the domain is released. */
  close(): Promise<void> {
    this.closing = true
    this.disposal ??= this.chain.then(() => this.domain.close())
    return this.disposal
  }
}

/** HTTP dependencies supplied by the authenticated Host composition. */
export interface IceSkyDraftRouteOptions {
  /** Admission runs before scope lookup, body reading or state access. */
  readonly admit: (request: IncomingMessage) => 401 | 403 | undefined
  /** Header-only existence check; never resumes a cold Agent. */
  readonly sessionExists: (sessionId: SessionId, signal: AbortSignal) => Promise<boolean>
  /** Maximum complete JSON request bytes. */
  readonly maxDraftBytes: number
}

class RequestFailure extends Error {
  constructor(readonly status: number, readonly code: string, message: string) { super(message) }
}

async function scopeOf(request: IncomingMessage, options: IceSkyDraftRouteOptions, signal: AbortSignal): Promise<IceSkyDraftScope> {
  const query = new URL(request.url ?? '/', 'http://localhost').searchParams
  const scopes = query.getAll('scope')
  if (scopes.length !== 1) throw new RequestFailure(400, 'invalid-scope', '请选择独立草稿或一个聊天。')
  const scope = scopes[0]
  if (scope === 'standalone') return brandString<IceSkyDraftScope>(scope)
  if (!scope.startsWith('session:')) throw new RequestFailure(400, 'invalid-scope', '草稿所属聊天无效。')
  const raw = scope.slice('session:'.length)
  if (!raw || raw.length > 512 || /[\x00-\x20/\\]/.test(raw)) throw new RequestFailure(400, 'invalid-scope', '草稿所属聊天无效。')
  if (!await options.sessionExists(SessionId(raw), signal)) throw new RequestFailure(404, 'session-not-found', '所属聊天不存在。')
  return brandString<IceSkyDraftScope>(scope)
}

async function readRequest(request: IncomingMessage, maxBytes: number): Promise<unknown> {
  const body = await readRequestBytes(request, maxBytes, () => new RequestFailure(413, 'draft-too-large', '草稿超过保存大小限制。'))
  try { return JSON.parse(body.toString('utf8')) }
  catch (_invalidDraft) { throw new RequestFailure(400, 'invalid-draft', '草稿 JSON 无效。') }
}

function json(response: ServerResponse, status: number, value: object): void {
  response.writeHead(status, { 'Content-Type': 'application/json; charset=utf-8', 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' })
  response.end(JSON.stringify(value))
}

/**
 * Create the production draft route with explicit Host admission and persistence ownership.
 * @param store - durable draft owner.
 * @param options - authenticated scope resolution and request budget.
 * @returns the handler for GET, PUT and DELETE on `/rainy/icesky/state`.
 */
export function createIceSkyDraftHandler(
  store: IceSkyDraftStore, options: IceSkyDraftRouteOptions,
): (request: IncomingMessage, response: ServerResponse) => Promise<void> {
  return async (request, response) => {
    const rejection = options.admit(request)
    if (rejection !== undefined) { response.writeHead(rejection); response.end(); return }
    if (!['GET', 'PUT', 'DELETE'].includes(request.method ?? '')) { response.writeHead(405, { Allow: 'GET, PUT, DELETE' }); response.end(); return }
    const controller = new AbortController()
    const disconnected = (): void => { controller.abort() }
    response.once('close', disconnected)
    try {
      const scope = await scopeOf(request, options, controller.signal)
      if (request.method === 'GET') { json(response, 200, store.get(scope)); return }
      const body = await readRequest(request, options.maxDraftBytes)
      const parsed = request.method === 'PUT' ? writeSchema.safeParse(body) : clearSchema.safeParse(body)
      if (!parsed.success) throw new RequestFailure(400, 'invalid-draft', '草稿只能保存文字和参数，不能包含 API 密钥。')
      const next = await store.replace(scope, parsed.data.baseRevision, parsed.data.data)
      json(response, 200, next)
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

/** Draft request budget, separate from model API relay limits. */
export interface Config {
  /** Maximum complete JSON request size, including all tool drafts and request metadata. */
  maxDraftBytes: number
}
/** Validated storage and request budget. */
export const Config: s<Config> = s.object({ maxDraftBytes: s.number().min(1024).max(64 * 1024 * 1024).default(8 * 1024 * 1024) })
/** Cordis plugin name. */
export const name = 'rainy-icesky-state'
/** Required Host owners for authenticated cold-readable drafts. */
export const inject = ['webServer', 'connection', 'storageDomain', 'agents', 'sessionPersistence']

/**
 * Mount one durable draft domain and its authenticated API.
 * @param ctx - Host composition with JSON-backed storage domains and Session metadata.
 * @param config - validated request budget.
 */
export async function apply(ctx: Context, config: Config): Promise<void> {
  const domain = await ctx.storageDomain.open(iceSkyDraftSpec)
  const store = new IceSkyDraftStore(domain)
  ctx.effect(() => () => store.close())
  const handler = createIceSkyDraftHandler(store, {
    admit: (request) => { const result = ctx.connection.admit(request); return 'rejection' in result ? result.rejection : undefined },
    maxDraftBytes: config.maxDraftBytes,
    sessionExists: async (id, signal) => ctx.agents.get(id) !== undefined
      || (await ctx.sessionPersistence.stat(id, { signal }))?.header.id === id,
  })
  ctx.effect(() => ctx.webServer.register({ kind: 'exact', path: '/rainy/icesky/state', handler }))
}
