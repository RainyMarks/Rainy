/** Per-project editor snapshots and dirty-buffer recovery under `<home>/ide`, separate from chat logs. */
import { createHash } from 'node:crypto'
import { join } from 'node:path'
import { z } from 'zod'
import { assertNever, brandString } from '../../shared/brand.ts'
import type {
  IdeFilesResults, IdeFileVersion, IdeRootId, IdeStateRequest, IdeWorkspaceSelection, IdeWorkspaceState, IdeWorkspaceStateData, WorkspaceId,
} from '../../shared/ide-files-protocol.ts'
import { readJson, writeJson } from '../files.ts'
import type { Projects } from '../projects.ts'
import { ideExecutionConfigurationSchema } from './execution-schema.ts'
import { IdeOperationError, ideRelativePath } from './files-core.ts'

const relativePath = z.string().max(32768).refine((value) => {
  try { ideRelativePath(value); return true } catch (_invalidPath) { return false }
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
  lastSessionId: z.string().min(1).max(512).nullable(),
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
  const identity = (entry: { rootId?: IdeRootId | undefined; path: string }): string => `${entry.rootId ?? 'primary'}:${entry.path}`
  const tabs = new Set(data.tabs.map(tab => `${tab.kind}:${identity(tab)}`))
  if (tabs.size !== data.tabs.length) context.addIssue({ code: 'custom', message: 'An editor tab may appear only once.' })
  if (new Set(data.buffers.map(identity)).size !== data.buffers.length) context.addIssue({ code: 'custom', message: 'A file may have only one recovery buffer.' })
  if (data.activePath !== null && !data.tabs.some(tab => identity(tab) === identity({ path: data.activePath ?? '', rootId: data.activeRootId }))) context.addIssue({ code: 'custom', message: 'The active path must name an open tab.' })
}) satisfies z.ZodType<IdeWorkspaceStateData>

const recordSchema = z.object({
  version: z.literal(1), revision: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER), data: ideStateDataSchema,
}).strict() satisfies z.ZodType<IdeWorkspaceState>
const selectionSchema = z.object({ version: z.literal(1), workspaceId: workspaceId.nullable() }).strict()

/** Recovery budgets and the initial layout of a project opened for the first time. */
export const ideStateConfigSchema = z.object({
  maxStateBytes: z.number().int().positive().default(32 * 1024 * 1024),
  maxBufferBytes: z.number().int().positive().default(5 * 1024 * 1024),
  maxTabs: z.number().int().positive().default(64),
  maxBuffers: z.number().int().positive().default(32),
  maxExpandedPaths: z.number().int().positive().default(10000),
  initialLayout: layoutSchema.default({ sidebarWidth: 240, agentWidth: 400, bottomHeight: 220,
    sidebarVisible: true, agentVisible: false, bottomVisible: false, bottomTab: 'terminal' }),
}).strict()

/** Resolved recovery budgets. */
export type IdeStateConfig = z.infer<typeof ideStateConfigSchema>

/**
 * Apply defaults to partial budgets.
 * @param config Overrides; omitted fields use the 1.x defaults.
 * @returns Complete state configuration.
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
 * Parse a state operation received over JSON.
 * @param value Decoded request.
 * @returns A strict state operation, never provider settings.
 */
export function parseIdeStateRequest(value: unknown): IdeStateRequest {
  const parsed = requestSchema.safeParse(value)
  if (!parsed.success) throw new IdeOperationError('invalid-request', 'The IDE state request contains invalid editor fields.')
  return parsed.data
}

/**
 * File name of one project's snapshot; identities that are not plain file names are hashed.
 * @param id Project identity.
 * @returns A file name inside the state directory.
 */
export function ideStateFileName(id: WorkspaceId): string {
  const plain = /^[A-Za-z0-9_-]{1,128}$/u.test(id) && !/^(?:con|prn|aux|nul|com\d|lpt\d)$/iu.test(id)
  return `${plain ? id : `sha256-${createHash('sha256').update(id).digest('hex')}`}.json`
}

/** Dependencies of {@link RainyIdeStateStore}. */
export interface RainyIdeStateOptions {
  /** Directory holding `<workspaceId>.json` snapshots and `selection.json`. */
  readonly directory: string
  readonly projects: Pick<Projects, 'get'>
  readonly config: IdeStateConfig
  /** Atomic JSON publication; defaults to {@link writeJson}. */
  readonly write?: (path: string, value: unknown) => Promise<void>
  /** Reports an unreadable selection file, which is then treated as no selection. */
  readonly log?: (message: string) => void
}

/** Serialized revision comparisons and atomic publication of per-project recovery state. */
export class RainyIdeStateStore {
  private chain: Promise<void> = Promise.resolve()
  private closing = false
  private disposal?: Promise<void>
  private readonly records = new Map<WorkspaceId, Promise<IdeWorkspaceState | undefined>>()
  private readonly write: (path: string, value: unknown) => Promise<void>

  private constructor(private readonly options: RainyIdeStateOptions, private selection: IdeWorkspaceSelection) {
    this.write = options.write ?? writeJson
  }

  /**
   * Load the remembered selection; project snapshots load on first use.
   * @param options State directory, project lookup and budgets.
   * @returns The store.
   */
  static async open(options: RainyIdeStateOptions): Promise<RainyIdeStateStore> {
    let selection: IdeWorkspaceSelection = { version: 1, workspaceId: null }
    try {
      const raw = await readJson(join(options.directory, 'selection.json'))
      if (raw !== undefined) selection = selectionSchema.parse(raw)
    } catch (error) {
      options.log?.(`IDE selection is unreadable and was ignored: ${error instanceof Error ? error.message : String(error)}`)
    }
    return new RainyIdeStateStore(options, selection)
  }

  /**
   * Read committed state, including recovery buffers when a directory is temporarily missing.
   * @param workspaceId Registered project identity.
   * @returns A detached committed snapshot, or revision-zero defaults.
   */
  async get(workspaceId: WorkspaceId): Promise<IdeWorkspaceState> {
    if (this.closing) throw new IdeOperationError('closed', 'IDE recovery storage is closing.')
    return structuredClone(await this.current(workspaceId))
  }

  /**
   * Read the last selected registered project.
   * @returns A detached selection; a project that is no longer registered reads as no selection.
   */
  getSelection(): IdeWorkspaceSelection {
    if (this.closing) throw new IdeOperationError('closed', 'IDE recovery storage is closing.')
    const id = this.selection.workspaceId
    return { version: 1, workspaceId: id !== null && this.options.projects.get(id) !== undefined ? id : null }
  }

  /**
   * Persist the selected project in admission order without changing recovery revisions.
   * @param workspaceId Registered project identity, or null to clear the selection.
   * @returns The selection after its atomic write succeeds.
   */
  setSelection(workspaceId: WorkspaceId | null): Promise<IdeWorkspaceSelection> {
    return this.serialize(async () => {
      if (workspaceId !== null && this.options.projects.get(workspaceId) === undefined) {
        throw new IdeOperationError('workspace-not-found', 'The selected workspace is no longer registered.')
      }
      const next: IdeWorkspaceSelection = { version: 1, workspaceId }
      await this.write(join(this.options.directory, 'selection.json'), next)
      this.selection = next
      return { ...next }
    })
  }

  /**
   * Save the complete editor snapshot only if its base revision is still the committed one.
   * @param workspaceId Registered project identity.
   * @param baseRevision Last committed revision observed by the client.
   * @param data Validated editor fields and dirty source copies.
   * @returns A detached snapshot after the atomic write succeeds.
   */
  replace(workspaceId: WorkspaceId, baseRevision: number, data: IdeWorkspaceStateData): Promise<IdeWorkspaceState> {
    const detached = structuredClone(data)
    return this.serialize(async () => {
      this.checkBudget(detached)
      const current = await this.current(workspaceId)
      if (current.revision !== baseRevision) {
        throw new IdeOperationError('revision-conflict', 'The workspace IDE state was saved by another client. Recover the current snapshot before replacing it.', { currentState: structuredClone(current) })
      }
      const next: IdeWorkspaceState = { version: 1, revision: baseRevision + 1, data: detached }
      await this.write(join(this.options.directory, ideStateFileName(workspaceId)), next)
      this.records.set(workspaceId, Promise.resolve(next))
      return structuredClone(next)
    })
  }

  /**
   * Run one parsed state operation.
   * @param request Validated state operation.
   * @returns The committed or loaded value.
   */
  handle<Request extends IdeStateRequest>(request: Request): Promise<IdeFilesResults[Request['op']]>
  handle(request: IdeStateRequest): Promise<IdeWorkspaceState | IdeWorkspaceSelection> {
    switch (request.op) {
      case 'state.read': return this.get(request.workspaceId)
      case 'state.save': return this.replace(request.workspaceId, request.baseRevision, request.data)
      case 'state.selection.read': return Promise.resolve().then(() => this.getSelection())
      case 'state.selection.save': return this.setSelection(request.workspaceId)
      default: return assertNever(request)
    }
  }

  /** @returns Completion after admitted writes drain; later operations are rejected. */
  close(): Promise<void> {
    this.closing = true
    this.disposal ??= this.chain
    return this.disposal
  }

  private serialize<T>(action: () => Promise<T>): Promise<T> {
    if (this.closing) return Promise.reject(new IdeOperationError('closed', 'IDE recovery storage is closing.'))
    const result = this.chain.then(action)
    this.chain = result.then(() => undefined, () => undefined)
    return result
  }

  private async current(workspaceId: WorkspaceId): Promise<IdeWorkspaceState> {
    if (this.options.projects.get(workspaceId) === undefined) throw new IdeOperationError('workspace-not-found', 'The selected workspace is no longer registered.')
    let record = this.records.get(workspaceId)
    if (record === undefined) {
      record = this.load(workspaceId)
      this.records.set(workspaceId, record)
      void record.catch(() => { if (this.records.get(workspaceId) === record) this.records.delete(workspaceId) })
    }
    return await record ?? { version: 1, revision: 0, data: {
      lastSessionId: null, tabs: [], activePath: null, expandedPaths: [], buffers: [],
      layout: structuredClone(this.options.config.initialLayout),
    } }
  }

  private async load(workspaceId: WorkspaceId): Promise<IdeWorkspaceState | undefined> {
    const unreadable = new IdeOperationError('io-error', 'The saved IDE state of this project has an unsupported format; it was left unchanged.')
    let raw: unknown
    try { raw = await readJson(join(this.options.directory, ideStateFileName(workspaceId))) } catch (error) {
      if (error instanceof SyntaxError) throw unreadable
      throw error
    }
    if (raw === undefined) return undefined
    const parsed = recordSchema.safeParse(raw)
    if (!parsed.success) throw unreadable
    return parsed.data
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
