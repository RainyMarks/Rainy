/** Workspace-scoped human file operations using the existing guarded local filesystem writer. */
import { createHash, randomUUID } from 'node:crypto'
import { execFile } from 'node:child_process'
import { lstat, mkdir, opendir, realpath, rmdir, unlink } from 'node:fs/promises'
import { basename, dirname, relative, resolve, sep } from 'node:path'
import { promisify } from 'node:util'
import { brandString } from '@deepseek-ai/dsh-brand'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { FileSystem, FsPathInfo, FsTarget, FsVersion } from '@deepseek-ai/dsh-fs'
import type { WorkspaceRegistry } from '@deepseek-ai/dsh-workspace'
import type {
  IdeDeletePreview, IdeDeleteToken, IdeDirectory, IdeFileChange, IdeFileDiff, IdeFileDocument,
  IdeFileEntry, IdeFilesRequest, IdeFilesResults, IdeFileVersion, IdeWorkspace, WorkspaceId,
  IdeRootId,
} from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'
import { z } from 'zod'
import { ideContains, IdeOperationError, ideRelativePath } from './ide-files-core.ts'
import { renameIdePathNoReplace } from './ide-files-native.ts'
import { searchIdeFiles } from './ide-files-search.ts'
import type RainyProjectRoots from './project-roots.ts'

/** Deployment budgets for human file and Git operations. */
export const ideFilesConfigSchema = z.object({
  maxTextBytes: z.number().int().positive().default(5 * 1024 * 1024),
  maxPreviewBytes: z.number().int().positive().default(64 * 1024),
  maxHexPreviewBytes: z.number().int().positive().default(4 * 1024),
  searchResultLimit: z.number().int().positive().default(200),
  searchMaxEntries: z.number().int().positive().default(50000),
  searchTimeoutMs: z.number().int().positive().default(3000),
  searchExcludedDirectories: z.array(z.string().min(1).regex(/^[^/\\]+$/u)).default(['.git', 'node_modules', '.venv', '__pycache__']),
  maxDirectoryEntries: z.number().int().positive().default(10000),
  maxDeleteEntries: z.number().int().positive().default(20000),
  maxDeletePreviews: z.number().int().positive().default(128),
  deletePreviewLifetimeMs: z.number().int().positive().default(120000),
  maxChangePaths: z.number().int().positive().default(256),
  gitTimeoutMs: z.number().int().positive().default(10000),
}).strict()

/** Resolved file-operation budgets. */
export type IdeFilesConfig = z.infer<typeof ideFilesConfigSchema>

/**
 * Resolve deployment defaults before constructing the service.
 * @param config - optional application overrides.
 * @returns validated complete budgets.
 */
export function resolveIdeFilesConfig(config: z.input<typeof ideFilesConfigSchema> = {}): IdeFilesConfig {
  return ideFilesConfigSchema.parse(config)
}

const workspaceIdSchema = z.string().min(1).max(512).transform(value => brandString<WorkspaceId>(value))
const versionSchema = z.string().min(1).max(1024).transform(value => brandString<IdeFileVersion>(value))
const pathSchema = z.string().max(32768)
const rootIdSchema = z.string().min(1).max(128).transform(value => brandString<IdeRootId>(value))
const rootLocation = { workspaceId: workspaceIdSchema, rootId: rootIdSchema.optional() }
const location = { ...rootLocation, path: pathSchema }
const filesRequestSchema = z.discriminatedUnion('op', [
  z.object({ op: z.literal('workspaces.list') }).strict(),
  z.object({ op: z.literal('workspaces.open'), path: pathSchema.min(1) }).strict(),
  z.object({ op: z.literal('workspaces.attach'), workspaceId: workspaceIdSchema, path: pathSchema.min(1) }).strict(),
  z.object({ op: z.literal('workspaces.removeRoot'), workspaceId: workspaceIdSchema, rootId: rootIdSchema }).strict(),
  z.object({ op: z.literal('files.list'), ...location }).strict(),
  z.object({ op: z.literal('files.read'), ...location }).strict(),
  z.object({ op: z.literal('files.search'), ...rootLocation, query: z.string().max(512), limit: z.number().int().positive().optional() }).strict(),
  z.object({ op: z.literal('files.save'), ...location, content: z.string(), expectedVersion: versionSchema }).strict(),
  z.object({ op: z.literal('files.create'), ...location, content: z.string() }).strict(),
  z.object({ op: z.literal('files.mkdir'), ...location }).strict(),
  z.object({ op: z.literal('files.rename'), ...location, destination: pathSchema, expectedVersion: versionSchema }).strict(),
  z.object({ op: z.literal('files.deletePreview'), ...location }).strict(),
  z.object({ op: z.literal('files.delete'), ...location, token: z.string().min(1).max(128).transform(value => brandString<IdeDeleteToken>(value)) }).strict(),
  z.object({ op: z.literal('files.diff'), ...location }).strict(),
  z.object({ op: z.literal('files.changes'), ...rootLocation, paths: z.array(pathSchema) }).strict(),
])

/**
 * Parse a file request received over JSON; unknown fields are rejected.
 * @param value - decoded request body.
 * @returns the validated request, with branded identities.
 */
export function parseIdeFilesRequest(value: unknown): IdeFilesRequest {
  const parsed = filesRequestSchema.safeParse(value)
  if (!parsed.success) throw new IdeOperationError('invalid-request', 'The IDE file request contains invalid fields.')
  return parsed.data
}

/** A registered directory after a fresh realpath and directory check. */
export interface ResolvedIdeWorkspace extends IdeWorkspace {
  readonly root: string
  readonly rootId?: IdeRootId | undefined
}

/** Explicit service dependencies; this service never opens an Agent or Session. */
export interface RainyIdeFilesOptions {
  readonly registry: Pick<WorkspaceRegistry, 'get' | 'list' | 'create'>
  readonly fs: Pick<FileSystem, 'resolve' | 'processPath' | 'lstat' | 'stat' | 'readBytes' | 'readByteRange' | 'writeText'>
  readonly config: IdeFilesConfig
  readonly roots: Pick<RainyProjectRoots, 'get' | 'resolveRoot' | 'attach' | 'remove'>
  /** Clock for confirmation expiry and cooperative search deadlines. */
  readonly now?: () => number
}

interface Observation {
  readonly absolute: string
  readonly target: FsTarget
  readonly info: FsPathInfo
  readonly targetVersion?: FsVersion
  readonly targetKind?: 'file' | 'directory' | 'other'
  readonly bytes: number
  readonly version: IdeFileVersion
  readonly outside: boolean
}

interface DeletionEntry {
  readonly path: string
  readonly version: FsVersion
  readonly kind: IdeFileEntry['kind']
  readonly bytes: number
  readonly identity: string
}

interface PendingDeletion {
  readonly workspaceId: WorkspaceId
  readonly root: string
  readonly path: string
  readonly expires: number
  readonly fingerprint: string
}

function versionOf(value: string): IdeFileVersion {
  return brandString<IdeFileVersion>(createHash('sha256').update(value).digest('hex'))
}

function deletionFingerprint(entries: readonly DeletionEntry[]): string {
  const hash = createHash('sha256')
  for (const entry of entries) hash.update(JSON.stringify(entry)).update('\n')
  return hash.digest('hex')
}

function workspaceView(workspace: { id: WorkspaceId; path: string; title: string }): IdeWorkspace {
  return { workspaceId: workspace.id, path: workspace.path, title: workspace.title }
}

function missing(error: unknown): boolean {
  return error !== null && typeof error === 'object' && 'code' in error && (error.code === 'ENOENT' || error.code === 'FS_NOT_FOUND')
}

function decode(bytes: Uint8Array, partial = false): Pick<IdeFileDocument, 'content' | 'bom' | 'eol' | 'readOnlyReason'> {
  const bom = bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf
  if ((bytes[0] === 0xff && bytes[1] === 0xfe) || (bytes[0] === 0xfe && bytes[1] === 0xff)) {
    return { content: null, bom: false, eol: 'lf', readOnlyReason: 'unsupported-encoding' }
  }
  if (bytes.includes(0)) return { content: null, bom, eol: 'lf', readOnlyReason: 'binary' }
  let content: string
  try { content = new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bom ? bytes.subarray(3) : bytes, { stream: partial }) }
  catch (_invalidUtf8) { return { content: null, bom, eol: 'lf', readOnlyReason: 'unsupported-encoding' } }
  const crlf = content.includes('\r\n')
  const lf = /(?<!\r)\n/u.test(content)
  return { content, bom, eol: crlf && lf ? 'mixed' : crlf ? 'crlf' : 'lf', readOnlyReason: null }
}

function hexPreview(bytes: Uint8Array, totalBytes: number, limit: number): NonNullable<IdeFileDocument['preview']> {
  const prefix = bytes.subarray(0, limit)
  const rows: string[] = []
  for (let offset = 0; offset < prefix.length; offset += 16) {
    const row = prefix.subarray(offset, offset + 16)
    const hex = Array.from(row, byte => byte.toString(16).padStart(2, '0')).join(' ')
    const ascii = Array.from(row, byte => byte >= 32 && byte <= 126 ? String.fromCharCode(byte) : '.').join('')
    rows.push(`${offset.toString(16).padStart(8, '0')}  ${hex.padEnd(47, ' ')}  |${ascii}|`)
  }
  return { kind: 'hex', text: rows.join('\n'), bytesRead: prefix.length, truncated: prefix.length < totalBytes }
}

/** Human filesystem owner; writes, rename and confirmed deletion serialize within each workspace. */
export class RainyIdeFiles {
  private readonly tails = new Map<WorkspaceId, Promise<void>>()
  private readonly inFlight = new Set<Promise<unknown>>()
  private readonly deletions = new Map<IdeDeleteToken, PendingDeletion>()
  private readonly now: () => number
  private closing = false
  private closed?: Promise<void>

  /** @param options - existing workspace registry, local filesystem owner and resolved budgets. */
  constructor(private readonly options: RainyIdeFilesOptions) {
    this.now = options.now ?? Date.now
  }

  /**
   * Resolve a registered workspace without opening its chats.
   * @param workspaceId - durable project identity.
   * @returns a freshly checked canonical directory usable by execution services.
   */
  async resolveWorkspace(workspaceId: WorkspaceId, rootId?: IdeRootId): Promise<ResolvedIdeWorkspace> {
    if (this.closing) throw new IdeOperationError('closed', 'The IDE file service is closing.')
    const workspace = this.options.registry.get(workspaceId)
    if (workspace === undefined) throw new IdeOperationError('workspace-not-found', 'The selected workspace is no longer registered.')
    try {
      const mounted = await this.options.roots.resolveRoot(workspaceId, rootId)
      return { ...workspaceView(workspace), path: mounted.path, root: mounted.path, rootId: mounted.rootId }
    } catch (error) {
      if (error instanceof IdeOperationError) throw error
      throw new IdeOperationError('workspace-unavailable', 'The workspace directory is unavailable.')
    }
  }

  /**
   * Dispatch one validated, authenticated human request.
   * @param request - parsed file request.
   * @param signal - request lifetime; cancellation before a mutation prevents publication.
   * @returns the result keyed by the request operation in IdeFilesResults.
   */
  handle<Request extends IdeFilesRequest>(request: Request, signal?: AbortSignal): Promise<IdeFilesResults[Request['op']]>
  handle(request: IdeFilesRequest, signal?: AbortSignal): Promise<IdeFilesResults[IdeFilesRequest['op']]> {
    if (this.closing) return Promise.reject(new IdeOperationError('closed', 'The IDE file service is closing.'))
    const task = this.dispatch(request, signal)
    this.inFlight.add(task)
    void task.then(() => { this.inFlight.delete(task) }, () => { this.inFlight.delete(task) })
    return task
  }

  /** @returns resolution after all admitted operations settle and confirmation tokens are retired. */
  close(): Promise<void> {
    this.closing = true
    this.deletions.clear()
    this.closed ??= Promise.allSettled([...this.inFlight]).then(() => { this.deletions.clear() })
    return this.closed
  }

  private async dispatch(request: IdeFilesRequest, signal?: AbortSignal): Promise<IdeFilesResults[IdeFilesRequest['op']]> {
    signal?.throwIfAborted()
    const view = (workspace: { id: WorkspaceId; path: string; title: string }): IdeWorkspace =>
      ({ ...workspaceView(workspace), roots: this.options.roots.get(workspace.id) })
    if (request.op === 'workspaces.list') return this.options.registry.list().map(view)
    if (request.op === 'workspaces.open') return view(await this.options.registry.create(request.path))
    if (request.op === 'workspaces.attach' || request.op === 'workspaces.removeRoot') {
      if (request.op === 'workspaces.attach') await this.options.roots.attach(request.workspaceId, request.path)
      else await this.options.roots.remove(request.workspaceId, request.rootId)
      const project = this.options.registry.get(request.workspaceId)
      if (project === undefined) throw new IdeOperationError('workspace-not-found', 'The selected project is no longer registered.')
      return view(project)
    }
    const workspace = await this.resolveWorkspace(request.workspaceId, request.rootId)
    signal?.throwIfAborted()
    switch (request.op) {
      case 'files.list': return this.list(workspace, request.path, signal)
      case 'files.read': return this.read(workspace, request.path, signal)
      case 'files.search': return searchIdeFiles(workspace.root, request.query, request.limit,
        this.options.config, signal, this.now)
      case 'files.diff': return this.diff(workspace, request.path, signal)
      case 'files.changes': return this.changes(workspace, request.paths, signal)
      case 'files.deletePreview': return this.previewDelete(workspace, request.path, signal)
      case 'files.save': return this.mutate(workspace.workspaceId, () => this.save(workspace, request, signal))
      case 'files.create': return this.mutate(workspace.workspaceId, () => this.create(workspace, request.path, request.content, signal))
      case 'files.mkdir': return this.mutate(workspace.workspaceId, async () => {
        signal?.throwIfAborted()
        const absolute = await this.prepareNew(workspace, request.path)
        signal?.throwIfAborted()
        await mkdir(absolute)
        return this.entry(request.path, await this.observe(workspace, request.path))
      })
      case 'files.rename': return this.mutate(workspace.workspaceId, () => this.renameEntry(workspace, request, signal))
      case 'files.delete': return this.mutate(workspace.workspaceId, () => this.deleteEntry(workspace, request.path, request.token, signal))
      default: return assertNever(request)
    }
  }

  private mutate<T>(workspaceId: WorkspaceId, action: () => Promise<T>): Promise<T> {
    const prior = this.tails.get(workspaceId) ?? Promise.resolve()
    const result = prior.then(action)
    const tail = result.then(() => undefined, () => undefined)
    this.tails.set(workspaceId, tail)
    void tail.then(() => { if (this.tails.get(workspaceId) === tail) this.tails.delete(workspaceId) })
    return result
  }

  private async checkedPath(workspace: ResolvedIdeWorkspace, path: string, allowRoot = false): Promise<string> {
    ideRelativePath(path, allowRoot)
    const absolute = resolve(workspace.root, path)
    const parent = path === '' ? workspace.root : await realpath(dirname(absolute))
    if (!ideContains(workspace.root, parent)) throw new IdeOperationError('outside-workspace', 'A path component points outside the selected workspace.')
    return absolute
  }

  private async observe(workspace: ResolvedIdeWorkspace, path: string, allowRoot = false, allowOutside = false): Promise<Observation> {
    const absolute = await this.checkedPath(workspace, path, allowRoot)
    const info = await this.options.fs.lstat(absolute)
    if (info === undefined) throw new IdeOperationError('not-found', 'The selected path no longer exists.')
    const target = await this.options.fs.resolve(absolute)
    const outside = !ideContains(workspace.root, this.options.fs.processPath(target))
    if (outside && !allowOutside) throw new IdeOperationError('outside-workspace', 'The selected link points outside the workspace.')
    const targetInfo = outside ? undefined : await this.options.fs.stat(target)
    const version = versionOf(JSON.stringify([absolute, info.version, target.targetKey, targetInfo?.version]))
    return { absolute, target, info, version, outside, bytes: targetInfo?.size ?? info.size ?? 0,
      ...targetInfo === undefined ? {} : { targetVersion: targetInfo.version, targetKind: targetInfo.type } }
  }

  private entry(path: string, observation: Observation): IdeFileEntry {
    return { name: basename(path), path, kind: observation.info.type, bytes: observation.bytes,
      version: observation.version, outsideWorkspace: observation.outside,
      ...observation.targetKind === undefined ? {} : { targetKind: observation.targetKind } }
  }

  private async directoryNames(absolute: string, limit: number, signal?: AbortSignal): Promise<string[]> {
    const directory = await opendir(absolute)
    const names: string[] = []
    try {
      for (;;) {
        signal?.throwIfAborted()
        const entry = await directory.read()
        if (entry === null) return names
        if (names.length === limit) throw new IdeOperationError('too-large', 'The directory contains more entries than the configured limit.')
        names.push(entry.name)
      }
    } finally { await directory.close() }
  }

  private async list(workspace: ResolvedIdeWorkspace, path: string, signal?: AbortSignal): Promise<IdeDirectory> {
    const directory = await this.observe(workspace, path, true)
    if (directory.targetKind !== 'directory') throw new IdeOperationError('not-directory', 'The selected path is not a directory.')
    const names = await this.directoryNames(this.options.fs.processPath(directory.target), this.options.config.maxDirectoryEntries, signal)
    const entries: IdeFileEntry[] = []
    for (const name of names.sort((a, b) => a.localeCompare(b))) {
      signal?.throwIfAborted()
      const child = path === '' ? name : `${path}/${name}`
      try { entries.push(this.entry(child, await this.observe(workspace, child, false, true))) }
      catch (error) {
        if (missing(error) || error instanceof IdeOperationError && error.code === 'not-found') continue
        throw error
      }
    }
    entries.sort((a, b) => Number(b.kind === 'directory') - Number(a.kind === 'directory') || a.name.localeCompare(b.name))
    return { path, entries }
  }

  private async read(workspace: ResolvedIdeWorkspace, path: string, signal?: AbortSignal): Promise<IdeFileDocument> {
    signal?.throwIfAborted()
    const before = await this.observe(workspace, path)
    if (before.targetKind !== 'file') throw new IdeOperationError('not-file', 'The selected path is not a regular file.')
    const base = { workspaceId: workspace.workspaceId, path, version: before.version, bytes: before.bytes }
    const large = before.bytes > this.options.config.maxTextBytes
    const bytes = large
      ? await this.options.fs.readByteRange(before.target, { offset: 0, length: this.options.config.maxPreviewBytes }, signal)
      : await this.options.fs.readBytes(before.target, signal, this.options.config.maxTextBytes)
    signal?.throwIfAborted()
    const after = await this.observe(workspace, path)
    if (before.version !== after.version) throw this.stale(after.version)
    const decoded = decode(bytes, large && bytes.length < before.bytes)
    if (large) {
      const preview = decoded.content === null
        ? hexPreview(bytes, before.bytes, this.options.config.maxHexPreviewBytes)
        : { kind: 'utf8' as const, text: decoded.content, bytesRead: bytes.length, truncated: bytes.length < before.bytes }
      return { ...base, ...decoded, content: null, readOnlyReason: decoded.readOnlyReason ?? 'too-large', preview }
    }
    return { ...base, bytes: bytes.length, ...decoded,
      ...decoded.content === null ? { preview: hexPreview(bytes, bytes.length, this.options.config.maxHexPreviewBytes) } : {} }
  }

  private stale(currentVersion: IdeFileVersion | null): IdeOperationError {
    return new IdeOperationError('version-conflict', 'The file changed after it was opened. Reload or compare before saving.', { currentVersion })
  }

  private checkText(content: string): void {
    if (!content.isWellFormed() || content.includes('\0')) throw new IdeOperationError('read-only', 'Only valid UTF-8 text can be saved by the editor.')
    if (Buffer.byteLength(content, 'utf8') > this.options.config.maxTextBytes) throw new IdeOperationError('too-large', 'The edited text exceeds the configured file limit.')
  }

  private async save(workspace: ResolvedIdeWorkspace, request: Extract<IdeFilesRequest, { op: 'files.save' }>, signal?: AbortSignal): Promise<IdeFileDocument> {
    let current: IdeFileDocument
    try { current = await this.read(workspace, request.path, signal) }
    catch (error) {
      if (missing(error) || error instanceof IdeOperationError && error.code === 'not-found') throw this.stale(null)
      throw error
    }
    if (current.version !== request.expectedVersion) throw this.stale(current.version)
    if (current.readOnlyReason !== null) throw new IdeOperationError('read-only', 'This file cannot be edited as bounded UTF-8 text.')
    const content = current.eol === 'crlf' ? request.content.replaceAll('\r\n', '\n').replaceAll('\n', '\r\n') : request.content
    const raw = `${current.bom ? '\uFEFF' : ''}${content}`
    this.checkText(raw)
    const observed = await this.observe(workspace, request.path)
    if (observed.version !== current.version || observed.targetVersion === undefined) throw this.stale(observed.version)
    await this.options.fs.writeText(observed.target, raw, { kind: 'replaceIfVersion', version: observed.targetVersion }, signal,
      { mode: 'workspace-write', workspaceRoot: workspace.root })
    return this.read(workspace, request.path, signal)
  }

  private async prepareNew(workspace: ResolvedIdeWorkspace, path: string): Promise<string> {
    const absolute = await this.checkedPath(workspace, path)
    if (await this.options.fs.lstat(absolute) !== undefined) throw new IdeOperationError('already-exists', 'The destination already exists.')
    return absolute
  }

  private async create(workspace: ResolvedIdeWorkspace, path: string, content: string, signal?: AbortSignal): Promise<IdeFileDocument> {
    this.checkText(content)
    signal?.throwIfAborted()
    const absolute = await this.prepareNew(workspace, path)
    const target = await this.options.fs.resolve(absolute)
    if (!ideContains(workspace.root, this.options.fs.processPath(target))) throw new IdeOperationError('outside-workspace', 'The destination points outside the selected workspace.')
    await this.options.fs.writeText(target, content, { kind: 'createIfAbsent' }, signal, { mode: 'workspace-write', workspaceRoot: workspace.root })
    return this.read(workspace, path, signal)
  }

  private async renameEntry(workspace: ResolvedIdeWorkspace, request: Extract<IdeFilesRequest, { op: 'files.rename' }>, signal?: AbortSignal): Promise<IdeFileEntry> {
    const source = await this.observe(workspace, request.path, false, true)
    if (source.version !== request.expectedVersion) throw this.stale(source.version)
    if (request.path === request.destination) return this.entry(request.path, source)
    const destination = await this.prepareNew(workspace, request.destination)
    if (source.info.type === 'directory' && ideContains(source.absolute, destination)) throw new IdeOperationError('invalid-path', 'A directory cannot be moved into itself.')
    signal?.throwIfAborted()
    const fresh = await this.observe(workspace, request.path, false, true)
    if (fresh.version !== source.version) throw this.stale(fresh.version)
    signal?.throwIfAborted()
    await renameIdePathNoReplace(fresh.absolute, destination)
    return this.entry(request.destination, await this.observe(workspace, request.destination, false, true))
  }

  private async deletionSnapshot(workspace: ResolvedIdeWorkspace, path: string, signal?: AbortSignal): Promise<DeletionEntry[]> {
    ideRelativePath(path)
    const pending = [path]
    const entries: DeletionEntry[] = []
    while (pending.length > 0) {
      signal?.throwIfAborted()
      const next = pending.pop()
      if (next === undefined) break
      const observed = await this.observe(workspace, next, false, true)
      const info = await lstat(observed.absolute, { bigint: true })
      entries.push({ path: next, version: observed.info.version, kind: observed.info.type, bytes: observed.info.type === 'file' ? observed.bytes : 0, identity: `${info.dev}:${info.ino}` })
      if (entries.length > this.options.config.maxDeleteEntries) throw new IdeOperationError('too-large', 'The selected tree exceeds the configured deletion limit.')
      if (observed.info.type === 'directory') {
        const names = await this.directoryNames(observed.absolute,
          this.options.config.maxDeleteEntries - entries.length - pending.length, signal)
        for (const name of names.sort().reverse()) pending.push(`${next}/${name}`)
      }
    }
    return entries
  }

  private async previewDelete(workspace: ResolvedIdeWorkspace, path: string, signal?: AbortSignal): Promise<IdeDeletePreview> {
    const entries = await this.deletionSnapshot(workspace, path, signal)
    const first = entries.at(0)
    if (first === undefined) throw new IdeOperationError('not-found', 'The selected path no longer exists.')
    const now = this.now()
    for (const [token, preview] of this.deletions) if (preview.expires <= now) this.deletions.delete(token)
    if (this.deletions.size >= this.options.config.maxDeletePreviews) {
      const oldest = this.deletions.keys().next().value
      if (oldest !== undefined) this.deletions.delete(oldest)
    }
    const token = brandString<IdeDeleteToken>(randomUUID())
    this.deletions.set(token, { workspaceId: workspace.workspaceId, root: workspace.root, path,
      expires: now + this.options.config.deletePreviewLifetimeMs, fingerprint: deletionFingerprint(entries) })
    return { path, kind: first.kind, entries: entries.length, bytes: entries.reduce((total, entry) => total + entry.bytes, 0), token }
  }

  private async deleteEntry(
    workspace: ResolvedIdeWorkspace, path: string, token: IdeDeleteToken, signal?: AbortSignal,
  ): Promise<{ path: string; deleted: true }> {
    const preview = this.deletions.get(token)
    this.deletions.delete(token)
    if (preview === undefined || preview.workspaceId !== workspace.workspaceId || preview.root !== workspace.root
      || preview.path !== path || preview.expires <= this.now()) {
      throw new IdeOperationError('confirmation-required', 'Confirm a fresh preview of this exact path before deleting it.')
    }
    const entries = await this.deletionSnapshot(workspace, path, signal)
    if (deletionFingerprint(entries) !== preview.fingerprint) throw new IdeOperationError('version-conflict', 'The selected tree changed after the deletion preview. Confirm the updated preview.')
    signal?.throwIfAborted()
    // No recursive filesystem remover is used: links are unlinked and directories must be empty.
    for (const entry of entries.reverse()) {
      const observed = await this.observe(workspace, entry.path, false, true)
      const info = await lstat(observed.absolute, { bigint: true })
      if (`${info.dev}:${info.ino}` !== entry.identity || observed.info.type !== entry.kind
        || entry.kind !== 'directory' && observed.info.version !== entry.version) throw this.stale(observed.version)
      if (entry.kind === 'directory') await rmdir(observed.absolute)
      else await unlink(observed.absolute)
    }
    return { path, deleted: true }
  }

  private async changes(
    workspace: ResolvedIdeWorkspace, paths: readonly string[], signal?: AbortSignal,
  ): Promise<readonly IdeFileChange[]> {
    if (paths.length > this.options.config.maxChangePaths) throw new IdeOperationError('too-large', 'Too many opened paths were requested in one change check.')
    const result: IdeFileChange[] = []
    for (const path of [...new Set(paths)]) {
      signal?.throwIfAborted()
      ideRelativePath(path, true)
      try {
        const value = await this.observe(workspace, path, true, true)
        result.push({ path, version: value.version, kind: value.info.type })
      } catch (error) {
        if (!missing(error) && !(error instanceof IdeOperationError && error.code === 'not-found')) throw error
        result.push({ path, version: null, kind: 'missing' })
      }
    }
    return result
  }

  private async git(root: string, args: string[], signal?: AbortSignal): Promise<Buffer> {
    const environment = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/KEY|SECRET|TOKEN|PASSWORD|^GIT_/iu.test(key)))
    const result = await promisify(execFile)('git', ['-c', 'core.fsmonitor=false', '-C', root, ...args], {
      encoding: 'buffer', timeout: this.options.config.gitTimeoutMs, maxBuffer: this.options.config.maxTextBytes,
      windowsHide: true, signal, env: { ...environment, GIT_OPTIONAL_LOCKS: '0', GIT_TERMINAL_PROMPT: '0' },
    })
    return result.stdout
  }

  private async diff(workspace: ResolvedIdeWorkspace, path: string, signal?: AbortSignal): Promise<IdeFileDiff> {
    ideRelativePath(path)
    let current: IdeFileDocument | undefined
    try { current = await this.read(workspace, path, signal) }
    catch (error) {
      if (!missing(error) && !(error instanceof IdeOperationError && error.code === 'not-found')) throw error
    }
    const partial = { path, current: current?.content ?? null, version: current?.version ?? null }
    if (current?.readOnlyReason !== null && current?.readOnlyReason !== undefined) return { ...partial, base: null, status: 'unavailable', reason: current.readOnlyReason }
    let repository: string
    try { repository = (await this.git(workspace.root, ['rev-parse', '--show-toplevel'], signal)).toString('utf8').trim() }
    catch (error) {
      signal?.throwIfAborted()
      if (error !== null && typeof error === 'object' && 'code' in error && error.code === 128) return { ...partial, base: null, status: 'unavailable', reason: 'not-git' }
      throw error
    }
    const repositoryPath = relative(repository, resolve(workspace.root, path)).split(sep).join('/')
    let tree: string
    try { tree = (await this.git(repository, ['ls-tree', '-z', 'HEAD', '--', repositoryPath], signal)).toString('utf8') }
    catch (error) {
      signal?.throwIfAborted()
      if (error !== null && typeof error === 'object' && 'code' in error && error.code === 128) return { ...partial, base: '', status: current === undefined ? 'unchanged' : 'added' }
      throw error
    }
    if (tree === '') return { ...partial, base: '', status: current === undefined ? 'unchanged' : 'added' }
    const objectId = /^\d+ blob ([a-f0-9]+)\t/u.exec(tree)?.[1]
    if (objectId === undefined) return { ...partial, base: null, status: 'unavailable', reason: 'binary' }
    const size = Number((await this.git(workspace.root, ['cat-file', '-s', objectId], signal)).toString('ascii').trim())
    if (!Number.isSafeInteger(size) || size > this.options.config.maxTextBytes) return { ...partial, base: null, status: 'unavailable', reason: 'too-large' }
    const decoded = decode(await this.git(workspace.root, ['cat-file', 'blob', objectId], signal))
    if (decoded.readOnlyReason !== null || decoded.content === null) {
      return { ...partial, base: null, status: 'unavailable', reason: decoded.readOnlyReason ?? 'unsupported-encoding' }
    }
    // Git stores LF; a CRLF checkout from core.autocrlf or eol attributes is not a content change.
    const base = current?.eol === 'crlf' && decoded.eol === 'lf' ? decoded.content.replaceAll('\n', '\r\n') : decoded.content
    return { ...partial, base, status: current === undefined ? 'deleted' : base === current.content ? 'unchanged' : 'modified' }
  }
}
