/** Optional parts of the carrier that are downloaded on first use instead of shipping in the installer. */
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, readdir, rename, rm } from 'node:fs/promises'
import { join } from 'node:path'
import { x as extractTar } from 'tar'
import { z } from 'zod'
import type { OptionalModuleId, OptionalModuleStatus, OptionalModulesState } from '@deepseek-ai/dsh-client-ui-rainy/modules-protocol'
import { downloadReleaseFile } from './release-download.ts'
import { assertToolPackPath, checkToolPackCancellation, toolPackStat, writeToolPackRecord } from './toolpack-files.ts'

const hash = z.string().regex(/^[a-f0-9]{64}$/)
const bytes = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER)
const moduleIds = ['strata', 'php', 'linux-runtime'] as const
/** Every module, including the Linux runtime that the carrier fetches by itself for a WSL execution target. */
export type CarrierModuleId = typeof moduleIds[number]
const descriptorSchema = z.object({ version: z.literal(1), modules: z.array(z.object({
  id: z.enum(moduleIds),
  // A directory module is unpacked; a file module stays an archive that another installer consumes.
  kind: z.enum(['directory', 'file']),
  file: z.string().regex(/^[a-z0-9][a-z0-9.-]*\.tar\.gz$/),
  bytes: bytes.refine(value => value > 0), sha256: hash, unpackedBytes: bytes,
  baseUrl: z.string().regex(/^https:\/\/github\.com\/RainyMarks\/RainyAgent\/releases\/download\/v\d+\.\d+\.\d+-resources\/$/),
  pieces: z.array(z.object({ file: z.string().regex(/^rainy-[a-f0-9]{20}\.\d{3}$/), bytes, sha256: hash }).strict()).min(1),
}).strict()) }).strict().superRefine((value, context) => {
  if (new Set(value.modules.map(module => module.id)).size !== value.modules.length) context.addIssue({ code: 'custom', message: 'Duplicate module' })
  for (const module of value.modules) {
    if (module.pieces.reduce((sum, piece) => sum + piece.bytes, 0) !== module.bytes) context.addIssue({ code: 'custom', message: `Piece sizes differ: ${module.id}` })
    if ((module.id === 'linux-runtime') !== (module.kind === 'file')) context.addIssue({ code: 'custom', message: `Unexpected module kind: ${module.id}` })
  }
})
type Descriptor = z.infer<typeof descriptorSchema>['modules'][number]
const markerSchema = z.object({ version: z.literal(1), id: z.enum(moduleIds), sha256: hash }).strict()
const MARKER = '.rainy-module.json'

/** Main-process locations and transport; the renderer cannot choose URLs or destinations. */
export interface OptionalModulesOptions {
  /** Signed carrier resource that pins every module archive. */
  readonly descriptorPath: string
  /** Per-user directory that receives the modules. */
  readonly root: string
  readonly fetch: typeof globalThis.fetch
  readonly publish: (state: OptionalModulesState) => void
}

/** Download, verify and unpack one module at a time; removal and replacement never touch a module that is in use. */
export class OptionalModules {
  private descriptors: Promise<Descriptor[]> | undefined
  private running: Promise<void> | undefined
  private abort: AbortController | undefined
  private state: OptionalModulesState = { phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' }
  private closed = false

  /** @param options - signed descriptor, destination and transport. */
  constructor(private readonly options: OptionalModulesOptions) {}

  private list(): Promise<Descriptor[]> {
    this.descriptors ??= readFile(this.options.descriptorPath, 'utf8').then(text => descriptorSchema.parse(JSON.parse(text)).modules)
    this.descriptors.catch(() => { this.descriptors = undefined })
    return this.descriptors
  }

  private async descriptor(id: CarrierModuleId): Promise<Descriptor> {
    const found = (await this.list()).find(module => module.id === id)
    if (found === undefined) throw new Error(`此版本未提供可下载的组件：${id}`)
    return found
  }

  /** Location of a module: its directory, or the archive of a file module.
   * @param id - module identity.
   * @returns absolute path, which exists only after installation.
   */
  async path(id: CarrierModuleId): Promise<string> {
    const module = await this.descriptor(id)
    return module.kind === 'directory' ? join(this.options.root, id) : join(this.options.root, id, module.file)
  }

  /** Whether the installed module matches the archive this carrier pins.
   * @param id - module identity.
   * @returns installation state.
   */
  async installed(id: CarrierModuleId): Promise<boolean> {
    const module = await this.descriptor(id)
    const marker = join(this.options.root, id, MARKER)
    await assertToolPackPath(marker)
    if (!await toolPackStat(marker)) return false
    const parsed = markerSchema.safeParse(JSON.parse(await readFile(marker, 'utf8')))
    return parsed.success && parsed.data.id === id && parsed.data.sha256 === module.sha256
      && (module.kind === 'directory' || (await toolPackStat(join(this.options.root, id, module.file)))?.isFile() === true)
  }

  /** User-facing modules and their sizes. @returns status of Strata and PHP. */
  async status(): Promise<OptionalModuleStatus[]> {
    const modules = (await this.list()).filter((module): module is Descriptor & { id: OptionalModuleId } => module.id !== 'linux-runtime')
    return Promise.all(modules.map(async module => ({ id: module.id, installed: await this.installed(module.id),
      downloadBytes: module.bytes, unpackedBytes: module.unpackedBytes })))
  }

  /** Progress of the latest operation. @returns retained state. */
  progress(): OptionalModulesState { return this.state }

  private publish(state: OptionalModulesState): void {
    this.state = state
    this.options.publish(state)
  }

  /** Download and install one module; an installed matching module needs no network.
   * @param id - module identity.
   * @param progress - optional observer of downloaded bytes, used while the carrier starts.
   * @returns settled installation.
   */
  install(id: CarrierModuleId, progress?: (completed: number, total: number) => void): Promise<void> {
    if (this.closed) return Promise.reject(new Error('RainyAgent 正在关闭'))
    if (this.running) return Promise.reject(new Error('另一个组件正在下载，请稍候'))
    this.abort = new AbortController()
    this.running = this.run(id, this.abort.signal, progress).finally(() => { this.running = undefined; this.abort = undefined })
    return this.running
  }

  private async run(id: CarrierModuleId, signal: AbortSignal, observe?: (completed: number, total: number) => void): Promise<void> {
    const module = await this.descriptor(id)
    const report = (phase: OptionalModulesState['phase'], completedBytes: number): void => {
      if (id !== 'linux-runtime') this.publish({ phase, module: id, completedBytes, totalBytes: module.bytes, error: '' })
      observe?.(completedBytes, module.bytes)
    }
    try {
      if (await this.installed(id)) { report('complete', module.bytes); return }
      const downloads = join(this.options.root, '.downloads')
      await assertToolPackPath(downloads)
      await mkdir(downloads, { recursive: true })
      report('downloading', 0)
      const archive = await downloadReleaseFile(module, { directory: downloads, fetch: this.options.fetch, signal,
        progress: (completed) => { report('downloading', completed) } })
      checkToolPackCancellation(signal)
      report('installing', module.bytes)
      const target = join(this.options.root, id)
      const staging = join(this.options.root, `.staging-${id}-${randomUUID()}`)
      await assertToolPackPath(staging)
      await mkdir(staging, { recursive: true })
      try {
        if (module.kind === 'directory') {
          await extractTar({ file: archive, cwd: staging, strict: true, preservePaths: false,
            filter: (_path, entry) => 'type' in entry && (entry.type === 'File' || entry.type === 'Directory') })
        } else await rename(archive, join(staging, module.file))
        await writeToolPackRecord(join(staging, MARKER), { version: 1, id, sha256: module.sha256 })
        checkToolPackCancellation(signal)
        const retired = join(this.options.root, `.retired-${id}-${randomUUID()}`)
        if (await toolPackStat(target)) await rename(target, retired)
        await rename(staging, target)
        await rm(retired, { recursive: true, force: true }).catch((error: unknown) => { console.error('An earlier module copy was not removed', error) })
      } finally { await rm(staging, { recursive: true, force: true }) }
      await rm(archive, { force: true })
      report('complete', module.bytes)
    } catch (error) {
      const cancelled = signal.aborted
      if (id !== 'linux-runtime') {
        this.publish({ ...this.state, module: id, phase: cancelled ? 'cancelled' : 'error',
          error: cancelled ? '' : error instanceof Error ? error.message : '组件下载失败，请重试' })
      }
      if (!cancelled || id === 'linux-runtime') throw error
    }
  }

  /** Delete an installed module; the caller stops the programs that use it first.
   * @param id - module identity.
   */
  async remove(id: CarrierModuleId): Promise<void> {
    if (this.running) throw new Error('请等待组件下载完成后再删除')
    await this.descriptor(id)
    const target = join(this.options.root, id)
    await assertToolPackPath(target)
    const retired = join(this.options.root, `.retired-${id}-${randomUUID()}`)
    if (await toolPackStat(target)) await rename(target, retired)
    await rm(retired, { recursive: true, force: true })
    if (id !== 'linux-runtime') this.publish({ phase: 'idle', module: id, completedBytes: 0, totalBytes: 0, error: '' })
  }

  /** Remove leftovers of interrupted installations and module archives this carrier no longer pins. */
  async clean(): Promise<void> {
    let names: string[]
    try { names = await readdir(this.options.root) } catch (error) {
      if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return
      throw error
    }
    if (this.running) return
    const pinned = new Set((await this.list()).flatMap(module => [module.file, `${module.file}.partial`, ...module.pieces.map(piece => piece.file)]))
    for (const name of names) {
      if (/^\.(?:staging|retired)-/.test(name)) await rm(join(this.options.root, name), { recursive: true, force: true })
    }
    const downloads = join(this.options.root, '.downloads')
    for (const name of (await toolPackStat(downloads))?.isDirectory() ? await readdir(downloads) : []) {
      if (!pinned.has(name)) await rm(join(downloads, name), { force: true })
    }
  }

  /** Abort the running download, keeping partial pieces for a later retry. @returns settled cancellation. */
  async cancel(): Promise<void> {
    this.abort?.abort()
    await this.running?.catch(() => { /* The failure was already published. */ })
  }

  /** Reject new work and finish cancellation before carrier exit. */
  async close(): Promise<void> { this.closed = true; await this.cancel() }
}
