/** Download selected tools into per-user storage from verified, resumable per-unit archives. */
import { mkdir, readFile, readdir, rm } from 'node:fs/promises'
import { join } from 'node:path'
import type { NativeToolId, NativeToolsDownloadState, NativeToolsOperation, NativeToolsUpdateState } from '../shared/native-tools-protocol.ts'
import type { ReleaseKeyring } from './release-trust.ts'
import { acquireToolPackLock, damagedToolPackUnits, installNativeToolPack, installedToolPackManifest, sameToolPackUnit } from './toolpack.ts'
import type { InstallNativeToolPackOptions, NativeToolPackInstallResult } from './toolpack.ts'
import { ToolPackInstallError, installedToolPackSchema } from './toolpack-format.ts'
import type { ToolPackMetadata } from './toolpack-format.ts'
import { assertToolPackPath, checkToolPackCancellation, readToolPackRecord, toolPackStat, writeToolPackFile } from './toolpack-files.ts'
import { TOOL_CHANNEL_URL, authenticateToolChannel } from './native-tools-update.ts'
import type { NativeToolsChannel } from './native-tools-update.ts'
import { parseNativeToolCatalog } from './native-tools.ts'
import { downloadReleaseFile } from './release-download.ts'
import { pruneToolPackBackups } from './toolpack-prune.ts'

const CHANNEL_FILE = 'channel.v2.signed.json'
type Catalog = ReturnType<typeof parseNativeToolCatalog>

/** Main-process paths and optional transport dependencies; renderer content cannot choose a URL or destination. */
export interface NativeToolsDownloadOptions {
  /** Per-user tool directory. */
  readonly onlineRoot: string
  /** Carrier directory whose offline-installed tools are updated in place instead of downloaded again. */
  readonly legacyRoot?: string
  /** The carrier already holds the legacy directory's installation lock. */
  readonly legacyLocked?: boolean
  readonly cacheRoot: string
  /** Signed channel shipped with this carrier. */
  readonly channelPath: string
  readonly keys: ReleaseKeyring
  readonly publish: (state: NativeToolsDownloadState) => void
  readonly fetch?: typeof globalThis.fetch
  readonly install?: (options: InstallNativeToolPackOptions) => Promise<NativeToolPackInstallResult>
}

/** One tool compared with the newest accepted catalog. */
export interface NativeToolState {
  readonly installed: boolean
  readonly outdated: boolean
  readonly downloadBytes: number
}

/** Where tools live, the catalogs that describe them, and the per-tool comparison. */
export interface NativeToolsInventory {
  readonly root: string
  /** Newest accepted catalog text, listing every downloadable tool. */
  readonly catalogText: string
  /** Catalog file of the installed pack; its entries match the installed files. */
  readonly installedCatalogText?: string
  readonly states: ReadonlyMap<string, NativeToolState>
  readonly catalogOutdated: boolean
  /** Compressed bytes that bring every installed unit, including tools replaced by a local override, to the newest catalog. */
  readonly updateBytes: number
}

/** A tool operation requested by the trusted window. */
export type NativeToolsRequest =
  | { readonly operation: 'install' | 'remove'; readonly tools: readonly NativeToolId[] }
  | { readonly operation: 'update' | 'repair' }

interface Installed {
  readonly metadata: ToolPackMetadata
  readonly units: readonly string[]
  readonly keys: ReadonlySet<string>
}

function unitsFor(catalog: Catalog, ids: Iterable<string>): string[] {
  const units = new Map<string, string>()
  for (const id of ids) for (const root of catalog.tools.find(tool => tool.id === id)?.roots ?? []) units.set(root.toLowerCase(), root)
  return [...units.values()]
}

async function optionalText(path: string): Promise<string | undefined> {
  await assertToolPackPath(path)
  return await toolPackStat(path) ? readFile(path, 'utf8') : undefined
}

/** Own one resumable tool operation at a time; closing waits for cancellation and rollback. */
export class NativeToolsDownloader {
  private state: NativeToolsDownloadState = { phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' }
  private running: Promise<void> | undefined
  private abort: AbortController | undefined
  private channel: NativeToolsChannel | undefined
  private loading: Promise<NativeToolsChannel> | undefined
  private installedCache: { root: string; packId: string; metadata: ToolPackMetadata } | undefined
  private closed = false
  private lastPublished = 0
  private checking: Promise<NativeToolsUpdateState> | undefined
  private checkAbort: AbortController | undefined
  private maintenance: Promise<number> | undefined
  private maintenanceAbort: AbortController | undefined

  /** @param options - trusted carrier paths, transport and retained progress owner. */
  constructor(private readonly options: NativeToolsDownloadOptions) {}

  /** Newest of the bundled and cached channels; a failed read is retried on the next request. */
  private current(): Promise<NativeToolsChannel> {
    if (this.channel) return Promise.resolve(this.channel)
    this.loading ??= this.load().finally(() => { this.loading = undefined })
    return this.loading
  }

  private async load(): Promise<NativeToolsChannel> {
    let selected = authenticateToolChannel(JSON.parse(await readFile(this.options.channelPath, 'utf8')), this.options.keys)
    const cached = join(this.options.cacheRoot, CHANNEL_FILE)
    const text = await optionalText(cached)
    // A cached revision that no longer verifies, for example after a key rotation, yields to the carrier's channel.
    if (text !== undefined) {
      try {
        const channel = authenticateToolChannel(JSON.parse(text), this.options.keys)
        if (channel.revision > selected.revision) selected = channel
      } catch (error) { console.error('The cached tool channel was ignored', error) }
    }
    this.channel = selected
    return selected
  }

  /** Directory that owns tools: the per-user root once used, otherwise an offline-installed carrier directory.
   * A carrier directory with a catalog but no installation record is listed as it is and never modified.
   */
  private async root(): Promise<{ path: string; lockHeld: boolean; managed: boolean }> {
    const used = async (root: string): Promise<boolean> => {
      for (const name of ['installed.json', 'journal.json']) {
        const path = join(root, '.rainy-toolpack', name)
        await assertToolPackPath(path)
        if (await toolPackStat(path)) return true
      }
      return false
    }
    const online = { path: this.options.onlineRoot, lockHeld: false, managed: true }
    if (await used(this.options.onlineRoot)) return online
    const legacy = this.options.legacyRoot
    if (legacy === undefined) return online
    if (await used(legacy)) return { path: legacy, lockHeld: this.options.legacyLocked ?? false, managed: true }
    const catalog = join(legacy, 'tools', 'manifest.json')
    await assertToolPackPath(catalog)
    return await toolPackStat(catalog) ? { path: legacy, lockHeld: false, managed: false } : online
  }

  private async installed(root: string): Promise<Installed | undefined> {
    const record = await readToolPackRecord(join(root, '.rainy-toolpack', 'installed.json'))
    if (record === undefined) return undefined
    const parsed = installedToolPackSchema.safeParse(record)
    if (!parsed.success) throw new ToolPackInstallError('invalid-record', '工具安装版本记录损坏，已保留原文件。')
    const cache = this.installedCache
    const metadata = cache?.root === root && cache.packId === parsed.data.packId ? cache.metadata
      : await installedToolPackManifest(root, parsed.data.packId)
    this.installedCache = { root, packId: parsed.data.packId, metadata }
    const units = parsed.data.version === 2 ? parsed.data.units : metadata.units.map(unit => unit.path)
    return { metadata, units, keys: new Set(units.map(unit => unit.toLowerCase())) }
  }

  /** Compare installed tools with the newest accepted catalog without accessing the network.
   * Without a usable channel, installed tools are still listed and nothing is offered for download.
   * @returns the tool directory, both catalogs and the per-tool state.
   */
  async inventory(): Promise<NativeToolsInventory> {
    let channel: NativeToolsChannel | undefined
    try { channel = await this.current() } catch (error) { console.error('The signed tool channel is unavailable', error) }
    const { path: root, managed } = await this.root()
    const installed = managed ? await this.installed(root) : undefined
    const installedCatalogText = managed && installed === undefined ? undefined : await optionalText(join(root, 'tools', 'manifest.json'))
    const present = new Set((installedCatalogText === undefined ? [] : parseNativeToolCatalog(installedCatalogText).tools)
      .map(tool => tool.id).filter(id => !managed || installed?.keys.has(`tools/${id}`)))
    const catalogText = channel?.catalog ?? installedCatalogText ?? '{"version":1,"tools":[]}'
    const archives = new Map(channel?.metadata.archives.map(archive => [archive.unit.toLowerCase(), archive.bytes]))
    const current = (unit: string): boolean => channel !== undefined && installed !== undefined && installed.keys.has(unit.toLowerCase())
      && sameToolPackUnit(installed.metadata, channel.metadata, unit)
    const states = new Map<string, NativeToolState>()
    for (const tool of parseNativeToolCatalog(catalogText).tools) {
      const stale = tool.roots.filter(unit => !current(unit))
      const outdated = managed && channel !== undefined && present.has(tool.id) && stale.length > 0
      states.set(tool.id, { installed: present.has(tool.id), outdated,
        downloadBytes: present.has(tool.id) && !outdated ? 0
          : stale.reduce((sum, unit) => sum + (archives.get(unit.toLowerCase()) ?? 0), 0) })
    }
    for (const id of present) if (!states.has(id)) states.set(id, { installed: true, outdated: false, downloadBytes: 0 })
    return { root, catalogText, ...installedCatalogText === undefined ? {} : { installedCatalogText }, states,
      catalogOutdated: channel !== undefined && installed !== undefined && !current('tools/manifest.json'),
      updateBytes: channel === undefined || installed === undefined ? 0 : channel.metadata.archives
        .filter(archive => installed.keys.has(archive.unit.toLowerCase()) && !current(archive.unit))
        .reduce((sum, archive) => sum + archive.bytes, 0) }
  }

  /** Check the fixed publisher channel; a failed check retains installed tools and the last accepted revision.
   * @returns whether installed tools differ from the newest catalog, or a recoverable check error.
   */
  checkUpdates(): Promise<NativeToolsUpdateState> {
    if (this.checking) return this.checking
    if (this.closed || this.running) return Promise.resolve({ phase: 'unchecked', version: this.channel?.releaseVersion ?? '', error: '' })
    this.checkAbort = new AbortController()
    const signal = AbortSignal.any([this.checkAbort.signal, AbortSignal.timeout(30_000)])
    this.checking = (async (): Promise<NativeToolsUpdateState> => {
      try {
        const accepted = await this.current()
        const response = await (this.options.fetch ?? globalThis.fetch)(TOOL_CHANNEL_URL, { signal, credentials: 'omit', cache: 'no-cache' })
        if (!response.ok || !response.body) { await response.body?.cancel(); throw new Error(`检查工具更新失败（HTTP ${response.status}）`) }
        const chunks: Buffer[] = []
        let size = 0
        for await (const chunk of response.body) {
          size += chunk.byteLength
          if (size > 12 * 1024 ** 2) throw new Error('工具更新目录超过大小限制')
          chunks.push(Buffer.from(chunk))
        }
        const text = Buffer.concat(chunks).toString('utf8')
        const channel = authenticateToolChannel(JSON.parse(text), this.options.keys)
        if (channel.revision === accepted.revision && channel.source.packId !== accepted.source.packId) throw new Error('工具更新清单冲突')
        // A publisher channel older than this carrier's own is ignored.
        if (channel.revision > accepted.revision) {
          await writeToolPackFile(join(this.options.cacheRoot, CHANNEL_FILE), text)
          this.channel = channel
        }
        const inventory = await this.inventory()
        const updatable = inventory.catalogOutdated || [...inventory.states.values()].some(state => state.outdated)
        return { phase: updatable ? 'available' : 'current', version: this.channel?.releaseVersion ?? channel.releaseVersion, error: '' }
      } catch (error) {
        if (this.checkAbort?.signal.aborted) return { phase: 'unchecked', version: this.channel?.releaseVersion ?? '', error: '' }
        return { phase: 'error', version: this.channel?.releaseVersion ?? '', error: error instanceof Error ? error.message : '无法检查工具更新，请检查网络后重试' }
      }
    })().finally(() => { this.checking = undefined; this.checkAbort = undefined })
    return this.checking
  }

  /** Return the progress of the latest operation without downloading. @returns current local state. */
  status(): NativeToolsDownloadState { return this.state }

  private publish(state: NativeToolsDownloadState): void {
    const changed = state.phase !== this.state.phase
    this.state = state
    const now = Date.now()
    if (!changed && (state.phase === 'downloading' || state.phase === 'installing') && now - this.lastPublished < 100) return
    this.lastPublished = now
    this.options.publish(state)
  }

  /** Run one tool operation; a second request while one runs is rejected.
   * @param request - tools to add or remove, an update of the installed tools, or a repair.
   * @returns settled operation.
   */
  start(request: NativeToolsRequest): Promise<void> {
    if (this.closed) return Promise.reject(new Error('RainyAgent 正在关闭'))
    if (this.running) return Promise.reject(new Error('另一个工具操作正在进行，请等待完成或先取消'))
    this.abort = new AbortController()
    this.running = this.run(request, this.abort.signal).finally(() => { this.running = undefined; this.abort = undefined })
    return this.running
  }

  /** Abort safely, retaining partial downloads and verified staging. @returns completion after rollback. */
  async cancel(): Promise<void> {
    this.checkAbort?.abort()
    if (!this.running) this.publish({ ...this.state, phase: 'cancelled', error: '' })
    this.abort?.abort()
    await this.running
  }

  /** Reject new operations and finish cancellation before carrier exit. @returns complete cleanup. */
  async close(): Promise<void> {
    this.closed = true
    this.checkAbort?.abort()
    this.maintenanceAbort?.abort()
    await this.cancel()
    await this.checking
    await this.maintenance
  }

  /** Remove backed-up program files from earlier upgrades and download files no current archive uses.
   * A tool operation started meanwhile stops the pruning, which resumes at the next call.
   * @returns reclaimed backup bytes.
   */
  maintain(): Promise<number> {
    if (this.maintenance) return this.maintenance
    if (this.closed || this.running) return Promise.resolve(0)
    const abort = new AbortController()
    this.maintenanceAbort = abort
    this.maintenance = (async () => {
      let removed = 0
      try {
        const legacy = this.options.legacyRoot
        if (legacy !== undefined && this.options.legacyLocked) {
          removed += (await pruneToolPackBackups(legacy, { signal: abort.signal })).removedBytes
        }
        if (await toolPackStat(join(this.options.onlineRoot, '.rainy-toolpack'))) {
          const release = await acquireToolPackLock(this.options.onlineRoot)
          try { removed += (await pruneToolPackBackups(this.options.onlineRoot, { signal: abort.signal })).removedBytes }
          finally { await release() }
        }
        await this.cleanCache()
      } catch (error) { if (!abort.signal.aborted) console.error('Tool storage maintenance failed', error) }
      return removed
    })().finally(() => { this.maintenance = undefined; this.maintenanceAbort = undefined })
    return this.maintenance
  }

  private async cleanCache(): Promise<void> {
    const channel = await this.current()
    const keep = new Set([CHANNEL_FILE, `metadata-${channel.metadata.id}.json`])
    for (const archive of channel.source.archives) for (const file of [archive.file, ...archive.pieces.map(piece => piece.file)]) keep.add(file).add(`${file}.partial`)
    let names: string[]
    try { names = await readdir(this.options.cacheRoot) } catch (error) {
      if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return
      throw error
    }
    for (const name of names) {
      const path = join(this.options.cacheRoot, name)
      if (!keep.has(name) && (await toolPackStat(path))?.isFile()) await rm(path)
    }
  }

  private async metadataFile(channel: NativeToolsChannel): Promise<string> {
    const path = join(this.options.cacheRoot, `metadata-${channel.metadata.id}.json`)
    await writeToolPackFile(path, JSON.stringify(channel.metadata))
    return path
  }

  private async run(request: NativeToolsRequest, signal: AbortSignal): Promise<void> {
    const operation: NativeToolsOperation = request.operation
    const tools = request.operation === 'install' || request.operation === 'remove' ? [...request.tools] : []
    const report = (phase: NativeToolsDownloadState['phase'], completedBytes = 0, totalBytes = 0): void => {
      this.publish({ phase, completedBytes, totalBytes, error: '', operation, tools })
    }
    try {
      this.maintenanceAbort?.abort()
      await this.maintenance
      await this.checking
      checkToolPackCancellation(signal)
      const channel = await this.current()
      const owner = await this.root()
      // A carrier directory without an installation record is never modified; downloads then start the per-user directory.
      const { path: root, lockHeld } = owner.managed ? owner : { path: this.options.onlineRoot, lockHeld: false }
      await assertToolPackPath(this.options.cacheRoot)
      await assertToolPackPath(root)
      await mkdir(this.options.cacheRoot, { recursive: true })
      await mkdir(root, { recursive: true })
      const installed = await this.installed(root)
      const installedIds = (catalog: Catalog): string[] => catalog.tools.map(tool => tool.id).filter(id => installed?.keys.has(`tools/${id}`))
      let metadata: ToolPackMetadata = channel.metadata
      let metadataPath: string
      let units: string[]
      let replace: string[] = []
      if (request.operation === 'remove') {
        const text = await optionalText(join(root, 'tools', 'manifest.json'))
        if (installed === undefined || text === undefined) throw new Error('工具尚未安装')
        const catalog = parseNativeToolCatalog(text)
        metadata = installed.metadata
        metadataPath = join(root, '.rainy-toolpack', 'manifests', `${metadata.id}.json`)
        const removed = new Set<string>(tools)
        units = unitsFor(catalog, installedIds(catalog).filter(id => !removed.has(id)))
      } else {
        const catalog = parseNativeToolCatalog(channel.catalog)
        for (const id of tools) if (!catalog.tools.some(tool => tool.id === id)) throw new Error(`工具不在当前目录中：${id}`)
        units = unitsFor(catalog, new Set([...installedIds(catalog), ...tools]))
        metadataPath = await this.metadataFile(channel)
        if (request.operation === 'repair' && installed !== undefined) {
          report('verifying')
          const kept = units.filter(unit => installed.keys.has(unit.toLowerCase()) && sameToolPackUnit(installed.metadata, metadata, unit))
          replace = await damagedToolPackUnits(root, metadata, kept, signal)
        }
      }
      const forced = new Set(replace.map(unit => unit.toLowerCase()))
      // The installer always brings the catalog files along with the selected tools.
      const wanted = new Set([...units, ...metadata.units.filter(unit => unit.kind === 'file').map(unit => unit.path)].map(unit => unit.toLowerCase()))
      const downloads = metadata.version === 1 || request.operation === 'remove' ? [] : metadata.archives
        .filter(archive => wanted.has(archive.unit.toLowerCase()) && (forced.has(archive.unit.toLowerCase())
          || installed === undefined || !installed.keys.has(archive.unit.toLowerCase())
          || !sameToolPackUnit(installed.metadata, metadata, archive.unit)))
        .map(archive => channel.source.archives.find(entry => entry.file === archive.file))
        .filter(archive => archive !== undefined)
      const totalBytes = downloads.reduce((sum, archive) => sum + archive.bytes, 0)
      let completed = 0
      if (totalBytes > 0) report('downloading', 0, totalBytes)
      for (const archive of downloads) {
        const baseline = completed
        await downloadReleaseFile(archive, { directory: this.options.cacheRoot, fetch: this.options.fetch ?? globalThis.fetch, signal,
          progress: (bytes) => { report('downloading', baseline + bytes, totalBytes) } })
        completed += archive.bytes
      }
      checkToolPackCancellation(signal)
      report('installing')
      await (this.options.install ?? installNativeToolPack)({ installRoot: root, mediaDirectory: this.options.cacheRoot, metadataPath,
        units, replace, lockHeld, signal,
        onProgress: (update) => { report('installing', update.completedBytes, update.totalBytes) },
      })
      for (const archive of downloads) await rm(join(this.options.cacheRoot, archive.file), { force: true })
      report('complete', totalBytes, totalBytes)
    } catch (error) {
      const recoveryFailed = error instanceof ToolPackInstallError && error.code === 'rollback-required'
      const cancelled = !recoveryFailed && (signal.aborted || error instanceof ToolPackInstallError && error.code === 'cancelled')
      this.publish({ ...this.state, operation, tools, phase: cancelled ? 'cancelled' : 'error',
        error: cancelled ? '' : error instanceof Error ? error.message : '工具操作失败，请重试' })
      if (!cancelled) throw error
    }
  }
}
