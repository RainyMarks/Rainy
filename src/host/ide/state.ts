/** Atomic workspace IDE snapshots and dirty-buffer recovery, separate from Session persistence. */
import { brandString } from '@deepseek-ai/dsh-brand'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import { defineDomain, domainTable } from '@deepseek-ai/dsh-storage-domain'
import type { Domain } from '@deepseek-ai/dsh-storage-domain'
import type { WorkspaceRegistry } from '@deepseek-ai/dsh-workspace'
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import type {
  IdeFilesResults, IdeFileVersion, IdeStateRequest, IdeWorkspaceSelection, IdeWorkspaceState, IdeWorkspaceStateData, WorkspaceId, IdeRootId,
} from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'
import { z } from 'zod'
import { IdeOperationError, ideRelativePath } from './ide-files-core.ts'
import { ideExecutionConfigurationSchema } from './ide-execution-schema.ts'

declare module '@deepseek-ai/cordis' {
  interface Context {
    /** Host-owned project selection shared with the native execution-target handoff. */
    rainyIdeState: Pick<RainyIdeStateStore, 'getSelection' | 'setSelection'>
  }
}

const relativePath = z.string().max(32768).refine((value) => {
  try { ideRelativePath(value); return true }
  catch (_invalidPath) { return false }
}, 'A recovery path must stay inside its workspace.')
const version = z.string().min(1).max(1024).transform(value => brandString<IdeFileVersion>(value))
const workspaceId = z.string().min(1).max(512).transform(value => brandString<WorkspaceId>(value))
const rootId = z.string().min(1).max(128).transform(value => brandString<IdeRootId>(value))
const eol = z.enum(['lf', 'crlf', 'mixed'])
const layoutSchema = z.object({
  sidebarWidth: z.number().nonnegative(), agentWidth: z.number().nonnegative(), bottomHeight: z.number().nonnegative(),
  sidebarVisible: z.boolean(), agentVisible: z.boolean(), bottomVisible: z.boolean(), bottomTab: z.enum(['terminal', 'problems', 'output', 'debug']),
}).strict()

/** Complete editor-state validator; strict fields exclude provider settings and credentials. */
export const ideStateDataSchema = z.object({
  lastSessionId: z.string().min(1).max(512).transform(value => brandString<SessionId>(value)).nullable(),
  tabs: z.array(z.object({ path: relativePath, rootId: rootId.optional(), kind: z.enum(['file', 'diff']),
    cursor: z.object({ line: z.number().int().positive(), column: z.number().int().positive() }).strict().optional(),
    scroll: z.object({ top: z.number().nonnegative(), left: z.number().nonnegative() }).strict().optional(),
  }).strict()),
  activePath: relativePath.nullable(),
  activeRootId: rootId.optional(),
  expandedPaths: z.array(relativePath),
  expandedRoots: z.array(z.object({ rootId, path: z.string().max(32768).refine((value) => {
    try { ideRelativePath(value, true); return true } catch (_invalidPath) { return false }
  }) }).strict()).optional(),
  buffers: z.array(z.object({ path: relativePath, rootId: rootId.optional(), content: z.string().refine(text => text.isWellFormed(), 'Recovery text must be valid Unicode.'),
    baseVersion: version.nullable(), bom: z.boolean(), eol }).strict()),
  layout: layoutSchema,
  execution: ideExecutionConfigurationSchema.optional(),
}).strict().superRefine((data, context) => {
  const identity = (entry: { rootId?: IdeRootId | undefined; path: string }) => `${entry.rootId ?? 'primary'}:${entry.path}`
  const tabs = new Set(data.tabs.map(tab => `${tab.kind}:${identity(tab)}`))
  if (tabs.size !== data.tabs.length) context.addIssue({ code: 'custom', message: 'An editor tab may appear only once.' })
  if (new Set(data.buffers.map(identity)).size !== data.buffers.length) context.addIssue({ code: 'custom', message: 'A file may have only one recovery buffer.' })
  if (data.activePath !== null && !data.tabs.some(tab => identity(tab) === identity({ path: data.activePath ?? '', rootId: data.activeRootId }))) context.addIssue({ code: 'custom', message: 'The active path must name an open tab.' })
}) satisfies z.ZodType<IdeWorkspaceStateData>

const recordSchema = z.object({
  version: z.literal(1), revision: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER), data: ideStateDataSchema,
}).strict() satisfies z.ZodType<IdeWorkspaceState>
const selectionSchema = z.object({ version: z.literal(1), workspaceId: workspaceId.nullable() }).strict()
const initialSelection: IdeWorkspaceSelection = { version: 1, workspaceId: null }

/** Workspace recovery rows use the existing atomic domain backend and their own format version. */
export const ideStateSpec = defineDomain({
  name: 'rainy_ide', version: 1, layout: 'single',
  global: { schema: selectionSchema, initial: initialSelection },
  tables: { workspaces: domainTable<WorkspaceId, IdeWorkspaceState>(recordSchema) },
})

/** Deployment budgets and the initial layout for previously unopened workspaces. */
export const ideStateConfigSchema = z.object({
  maxStateBytes: z.number().int().positive().default(32 * 1024 * 1024),
  maxBufferBytes: z.number().int().positive().default(5 * 1024 * 1024),
  maxTabs: z.number().int().positive().default(64),
  maxBuffers: z.number().int().positive().default(32),
  maxExpandedPaths: z.number().int().positive().default(10000),
  initialLayout: layoutSchema.default({ sidebarWidth: 240, agentWidth: 400, bottomHeight: 220,
    sidebarVisible: true, agentVisible: false, bottomVisible: false, bottomTab: 'terminal' }),
}).strict()

/** Resolved budgets used before a snapshot enters the durable write queue. */
export type IdeStateConfig = z.infer<typeof ideStateConfigSchema>

/**
 * Resolve state defaults at application construction.
 * @param config - application overrides.
 * @returns validated complete state configuration.
 */
export function resolveIdeStateConfig(config: z.input<typeof ideStateConfigSchema> = {}): IdeStateConfig {
  return ideStateConfigSchema.parse(config)
}

const requestSchema = z.discriminatedUnion('op', [
  z.object({ op: z.literal('state.read'), workspaceId }).strict(),
  z.object({ op: z.literal('state.save'), workspaceId,
    baseRevision: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER - 1), data: ideStateDataSchema,
  }).strict(),
  z.object({ op: z.literal('state.selection.read') }).strict(),
  z.object({ op: z.literal('state.selection.save'), workspaceId: workspaceId.nullable() }).strict(),
])

/**
 * Parse persisted editor data received over the IDE route.
 * @param value - decoded JSON body.
 * @returns a strict state operation, never provider settings.
 */
export function parseIdeStateRequest(value: unknown): IdeStateRequest {
  const parsed = requestSchema.safeParse(value)
  if (!parsed.success) throw new IdeOperationError('invalid-request', 'The IDE state request contains invalid editor fields.')
  return parsed.data
}

/** Dependencies for one exclusively owned IDE domain. */
export interface RainyIdeStateOptions {
  readonly domain: Domain<typeof ideStateSpec>
  readonly registry: Pick<WorkspaceRegistry, 'get'>
  readonly config: IdeStateConfig
}

/** Serialized revision comparisons and atomic durable publication for workspace recovery. */
export class RainyIdeStateStore {
  private chain: Promise<void> = Promise.resolve()
  private closing = false
  private disposal?: Promise<void>

  /** @param options - exclusively owned domain, workspace identity lookup, and resolved limits. */
  constructor(private readonly options: RainyIdeStateOptions) {}

  /**
   * Read committed state, including recovery buffers when a directory is temporarily missing.
   * @param workspaceId - registered project identity.
   * @returns a detached committed snapshot or revision-zero defaults.
   */
  get(workspaceId: WorkspaceId): IdeWorkspaceState {
    if (this.closing) throw new IdeOperationError('closed', 'IDE recovery storage is closing.')
    return this.current(workspaceId)
  }

  /**
   * Read the last selected registered project; missing older records and deleted registrations return null.
   * @returns a detached selection, retaining registered projects whose directory is temporarily unavailable.
   */
  getSelection(): IdeWorkspaceSelection {
    if (this.closing) throw new IdeOperationError('closed', 'IDE recovery storage is closing.')
    const current = this.options.domain.global.get()
    return { version: 1, workspaceId: current.workspaceId !== null && this.options.registry.get(current.workspaceId) !== undefined
      ? current.workspaceId : null }
  }

  /**
   * Persist the selected project in Host admission order without changing workspace recovery revisions.
   * @param workspaceId - registered project identity, or null to clear the remembered selection.
   * @returns the selected identity after its atomic write succeeds.
   */
  setSelection(workspaceId: WorkspaceId | null): Promise<IdeWorkspaceSelection> {
    if (this.closing) return Promise.reject(new IdeOperationError('closed', 'IDE recovery storage is closing.'))
    const request = this.chain.then(async () => {
      if (workspaceId !== null && this.options.registry.get(workspaceId) === undefined)
        throw new IdeOperationError('workspace-not-found', 'The selected workspace is no longer registered.')
      const next: IdeWorkspaceSelection = { version: 1, workspaceId }
      await this.options.domain.global.set(next)
      return { ...next }
    })
    this.chain = request.then(() => undefined, () => undefined)
    return request
  }

  /**
   * Save the complete editor snapshot only if its base revision still owns the record.
   * @param workspaceId - registered project identity.
   * @param baseRevision - last committed revision observed by this client.
   * @param data - validated editor fields and dirty source copies.
   * @returns a detached snapshot after the atomic durable commit succeeds.
   */
  replace(workspaceId: WorkspaceId, baseRevision: number, data: IdeWorkspaceStateData): Promise<IdeWorkspaceState> {
    if (this.closing) return Promise.reject(new IdeOperationError('closed', 'IDE recovery storage is closing.'))
    const detached = structuredClone(data)
    const request = this.chain.then(async () => {
      this.checkBudget(detached)
      const current = this.current(workspaceId)
      if (current.revision !== baseRevision) throw new IdeOperationError('revision-conflict', 'The workspace IDE state was saved by another client. Recover the current snapshot before replacing it.', { currentState: current })
      const next: IdeWorkspaceState = { version: 1, revision: baseRevision + 1, data: detached }
      await this.options.domain.table('workspaces').put(workspaceId, next)
      return structuredClone(next)
    })
    this.chain = request.then(() => undefined, () => undefined)
    return request
  }

  /**
   * Dispatch the parsed state operation without creating a Session.
   * @param request - validated JSON state operation.
   * @returns the committed or loaded snapshot.
   */
  handle<Request extends IdeStateRequest>(request: Request): Promise<IdeFilesResults[Request['op']]>
  handle(request: IdeStateRequest): Promise<IdeWorkspaceState | IdeWorkspaceSelection> {
    switch (request.op) {
      case 'state.read': return Promise.resolve().then(() => this.get(request.workspaceId))
      case 'state.save': return this.replace(request.workspaceId, request.baseRevision, request.data)
      case 'state.selection.read': return Promise.resolve().then(() => this.getSelection())
      case 'state.selection.save': return this.setSelection(request.workspaceId)
      default: return assertNever(request)
    }
  }

  /** @returns resolution after admitted writes drain and the owned domain closes. */
  close(): Promise<void> {
    this.closing = true
    this.disposal ??= this.chain.then(() => this.options.domain.close())
    return this.disposal
  }

  private current(workspaceId: WorkspaceId): IdeWorkspaceState {
    if (this.options.registry.get(workspaceId) === undefined) throw new IdeOperationError('workspace-not-found', 'The selected workspace is no longer registered.')
    const stored = this.options.domain.table('workspaces').get(workspaceId)
    return stored === undefined ? { version: 1, revision: 0, data: {
      lastSessionId: null, tabs: [], activePath: null, expandedPaths: [], buffers: [],
      layout: structuredClone(this.options.config.initialLayout),
    } } : structuredClone(stored)
  }

  private checkBudget(data: IdeWorkspaceStateData): void {
    const config = this.options.config
    if (data.tabs.length > config.maxTabs || data.buffers.length > config.maxBuffers
      || data.expandedPaths.length + (data.expandedRoots?.length ?? 0) > config.maxExpandedPaths
      || data.buffers.some(buffer => Buffer.byteLength(buffer.content, 'utf8') > config.maxBufferBytes)
      || Buffer.byteLength(JSON.stringify(data), 'utf8') > config.maxStateBytes) {
      throw new IdeOperationError('state-too-large', 'The workspace recovery snapshot exceeds the configured storage budget.')
    }
  }
}
