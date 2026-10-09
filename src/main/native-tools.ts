/** Resolve the installed human tool catalog and retain per-user catalog preferences. */
import { dirname, isAbsolute, relative, resolve, sep, win32 } from 'node:path'
import { createHash, randomUUID } from 'node:crypto'
import { z } from 'zod'
import { nativeToolIds } from '../shared/native-tools-protocol.ts'
import type { NativeToolCatalog, NativeToolId, NativeToolLaunchResult } from '../shared/native-tools-protocol.ts'
import { renameToolPackPath } from './toolpack-files.ts'
import { toolPackFileSystem } from './toolpack-fs.ts'

const { readFile, writeFile, mkdir, realpath, stat, rm } = toolPackFileSystem.promises

const ids = nativeToolIds
const toolIdSchema = z.string().regex(/^[a-z0-9][a-z0-9-]{0,63}$/)
  .refine(value => value !== 'burp-community' && !/^(con|prn|aux|nul|com[1-9]|lpt[1-9])$/.test(value)).transform(value => value as NativeToolId)
const storedToolIdSchema = z.union([toolIdSchema, z.literal('burp-community')])
const relativePath = z.string().min(1).refine(value => !value.includes('\0') && !value.includes('\\')
  && !isAbsolute(value) && !win32.isAbsolute(value) && !value.split('/').some(part => part === '..' || part === '.' || part === '')
  && !value.includes(':'), 'Expected an installation-relative path')
const entrySchema = z.object({
  kind: z.enum(['gui', 'console', 'java', 'web']), path: relativePath, cwd: relativePath,
  args: z.array(z.string().refine(value => !value.includes('\0'))).default([]), runtime: relativePath.optional(),
  dotnetRoot: relativePath.optional(),
  pythonRoot: relativePath.optional(),
  requiredFiles: z.array(relativePath).optional(),
})
const toolSchema = z.object({
  id: storedToolIdSchema, category: z.enum(['web', 'misc', 'reverse']), name: z.string().min(1),
  version: z.string(), entry: entrySchema, roots: z.array(relativePath).min(1),
  variants: z.array(z.object({ id: z.literal('x32'), name: z.string(), entry: entrySchema })).optional(),
  preserve: z.array(relativePath).optional(),
})
const catalogSchema = z.object({ version: z.literal(1), tools: z.array(toolSchema) }).superRefine((catalog, ctx) => {
  const seen = new Set<string>()
  for (const tool of catalog.tools) {
    if (seen.has(tool.id)) ctx.addIssue({ code: 'custom', message: `Duplicate tool: ${tool.id}` })
    seen.add(tool.id)
    if (tool.variants?.length && tool.id !== 'x64dbg') ctx.addIssue({ code: 'custom', message: 'Only x64dbg declares an x32 variant' })
    if (tool.variants && tool.variants.length > 1) ctx.addIssue({ code: 'custom', message: 'Duplicate x32 variant' })
    for (const entry of [tool.entry, ...(tool.variants?.map(variant => variant.entry) ?? [])]) {
      if (!entry.path.startsWith(`tools/${tool.id}/`) || !(entry.cwd === `tools/${tool.id}` || entry.cwd.startsWith(`tools/${tool.id}/`))
        || entry.kind === 'java' && entry.runtime === undefined || entry.runtime && !entry.runtime.startsWith('runtime/windows/')) {
        ctx.addIssue({ code: 'custom', message: `Invalid installed entry for ${tool.id}` })
      }
      if (entry.dotnetRoot && (!entry.dotnetRoot.startsWith('runtime/windows/') || !tool.roots.includes(entry.dotnetRoot))) {
        ctx.addIssue({ code: 'custom', message: `Invalid private .NET runtime for ${tool.id}` })
      }
      if (entry.pythonRoot && !entry.pythonRoot.startsWith(`tools/${tool.id}/`)) {
        ctx.addIssue({ code: 'custom', message: `Invalid private Python runtime for ${tool.id}` })
      }
      for (const file of entry.requiredFiles ?? []) if (!tool.roots.some(root => file.startsWith(root + '/'))) {
        ctx.addIssue({ code: 'custom', message: `Required file is outside the installed roots for ${tool.id}` })
      }
    }
    for (const root of tool.roots) if (root !== `tools/${tool.id}` && !root.startsWith(`tools/${tool.id}/`) && !root.startsWith('runtime/windows/')) {
      ctx.addIssue({ code: 'custom', message: `Invalid dependency root for ${tool.id}` })
    }
  }
})
const preferencesSchema = z.object({ version: z.literal(1),
  favorites: z.array(storedToolIdSchema).max(256), recent: z.array(storedToolIdSchema).max(256) })
const verificationSchema = z.object({ version: z.literal(1),
  catalogSha256: z.string().regex(/^[a-f0-9]{64}$/), tools: z.array(storedToolIdSchema) })
const localCatalogSchema = z.object({ version: z.literal(1), root: z.string()
  .refine(value => (isAbsolute(value) || win32.isAbsolute(value)) && !value.includes('\0')), catalog: catalogSchema }).strict()

/** Catalog entry loaded from the installed tool pack. */
export type InstalledNativeTool = Omit<z.infer<typeof toolSchema>, 'id'> & { id: NativeToolId }

type CurrentPreferences = { version: 1; favorites: NativeToolId[]; recent: NativeToolId[] }

/** A checked, absolute invocation; only the main process constructs it. */
export interface NativeInvocation {
  readonly id: NativeToolId
  readonly name: string
  readonly kind: InstalledNativeTool['entry']['kind']
  readonly target: string
  readonly executable: string
  readonly cwd: string
  readonly args: readonly string[]
  readonly roots: readonly string[]
  /** Private directory for the selected tool's user configuration. */
  readonly userData: string
  readonly dotnetRoot?: string
  readonly pythonRoot?: string
}

/** Installation of one tool compared with the newest catalog. */
interface ToolState { readonly installed: boolean; readonly outdated: boolean; readonly downloadBytes: number }

/** Installed and downloadable tools reported by the tool downloader. */
export interface NativeToolsInventorySource {
  readonly root: string
  readonly catalogText: string
  readonly installedCatalogText?: string
  readonly states: ReadonlyMap<string, ToolState>
  readonly catalogOutdated: boolean
  readonly updateBytes: number
}

/** Main-process dependencies for the installed catalog. */
export interface NativeToolsOptions {
  readonly installRoot: string
  readonly userData: string
  /** Catalog read when the installation has none and no inventory is supplied. */
  readonly catalogPath?: string
  /** Per-tool installation state; without it every catalog tool is treated as installed below installRoot. */
  readonly inventory?: () => Promise<NativeToolsInventorySource>
  /** Starts the checked invocation and resolves when the operating system accepts it. */
  readonly start: (invocation: NativeInvocation) => Promise<void>
}

/**
 * Parse a launch request at the renderer IPC boundary.
 * @param value - renderer-supplied request.
 * @returns the fixed tool and optional x32 selection.
 */
export function parseNativeLaunch(value: unknown): { readonly id: NativeToolId; readonly variant?: 'x32' } {
  return z.object({ id: toolIdSchema, variant: z.literal('x32').optional() }).strict().refine(value => value.variant === undefined || value.id === 'x64dbg').parse(value)
}

/**
 * Parse a favorite list at the renderer IPC boundary.
 * @param value - renderer-supplied tool identities.
 * @returns deduplicated catalog identities.
 */
export function parseNativeFavorites(value: unknown): NativeToolId[] {
  return [...new Set(z.array(toolIdSchema).max(256).parse(value))]
}

/** Validate a publisher catalog before caching or displaying it. @param text - JSON catalog. @returns validated catalog. */
export function parseNativeToolCatalog(text: string): z.infer<typeof catalogSchema> { return catalogSchema.parse(JSON.parse(text)) }

function inside(root: string, path: string): boolean {
  const suffix = relative(root, path)
  return suffix === '' || (!suffix.startsWith(`..${sep}`) && suffix !== '..' && !isAbsolute(suffix))
}

/**
 * Resolve an existing file or directory without following links outside the installation.
 * @param root - installed application directory.
 * @param child - validated relative entry.
 * @returns the actual contained path.
 */
export async function resolveNativePath(root: string, child: string): Promise<string> {
  relativePath.parse(child)
  const actualRoot = await realpath(root)
  const actual = await realpath(resolve(root, child))
  if (!inside(actualRoot, actual)) throw new Error(`工具路径超出安装目录：${child}`)
  return actual
}

async function optionalJson(path: string, unavailableParentIsMissing = false): Promise<unknown> {
  try { return JSON.parse(await readFile(path, 'utf8')) }
  catch (error) {
    if (error instanceof Error && 'code' in error && (error.code === 'ENOENT' || unavailableParentIsMissing && error.code === 'ENOTDIR')) return undefined
    throw error
  }
}

/** The user's catalog preferences and installed launch entries. */
export class NativeToolsLibrary {
  private mutation: Promise<void> = Promise.resolve()
  private readonly pending = new Map<string, Promise<NativeToolLaunchResult>>()
  private readonly preferencesPath: string

  /** @param options - installation paths and native launch owner. */
  constructor(private readonly options: NativeToolsOptions) {
    this.preferencesPath = resolve(options.userData, 'native-tools.json')
  }

  private async catalog(): Promise<{
    readonly tools: (InstalledNativeTool & { readonly installRoot: string; readonly state?: ToolState })[]
    readonly verified: ReadonlySet<string>
    readonly catalogOutdated: boolean
    readonly updateBytes: number
  }> {
    const inventory = await this.options.inventory?.()
    const root = inventory?.root ?? this.options.installRoot
    let text = inventory?.installedCatalogText
    if (inventory === undefined) {
      try { text = await readFile(resolve(root, 'tools/manifest.json'), 'utf8') }
      catch (error) {
        if (!(error instanceof Error) || !('code' in error) || error.code !== 'ENOENT' || !this.options.catalogPath) throw error
        text = await readFile(this.options.catalogPath, 'utf8')
      }
    }
    const installed = text === undefined ? [] : parseNativeToolCatalog(text).tools
    const verified = new Set<string>()
    const record = text === undefined ? undefined : await optionalJson(resolve(root, 'tools/verified.json'))
    if (record !== undefined) {
      const parsed = verificationSchema.parse(record)
      if (parsed.catalogSha256 === createHash('sha256').update(text ?? '').digest('hex')) for (const id of parsed.tools) verified.add(id)
    }
    // Installed tools launch with the entries of their own catalog; the others are listed from the newest catalog.
    let listed = installed
    if (inventory !== undefined) {
      const newest = parseNativeToolCatalog(inventory.catalogText).tools
      const present = (id: string): boolean => inventory.states.get(id)?.installed ?? false
      listed = [...newest.map(tool => present(tool.id) ? installed.find(entry => entry.id === tool.id) ?? tool : tool),
        ...installed.filter(tool => present(tool.id) && !newest.some(entry => entry.id === tool.id))]
    }
    const tools = listed.flatMap((tool) => {
      const { id } = tool
      const state = inventory?.states.get(id)
      return id === 'burp-community' ? [] : [{ ...tool, id, installRoot: root, ...state === undefined ? {} : { state } }]
    })
    const local = await optionalJson(resolve(this.options.userData, 'native-tools.local.json'), true)
    if (local !== undefined) {
      const selected = localCatalogSchema.parse(local)
      for (const tool of selected.catalog.tools) {
        if (tool.id === 'burp-community') continue
        const index = tools.findIndex(value => value.id === tool.id)
        const replacement = { ...tool, id: tool.id, installRoot: selected.root }
        if (index < 0) tools.push(replacement)
        else tools[index] = replacement
        verified.delete(tool.id)
      }
    }
    return { tools, verified, catalogOutdated: inventory?.catalogOutdated ?? false, updateBytes: inventory?.updateBytes ?? 0 }
  }

  private async preferences(): Promise<CurrentPreferences> {
    const value = await optionalJson(this.preferencesPath)
    if (value === undefined) return { version: 1, favorites: [], recent: [] }
    const stored = preferencesSchema.parse(value)
    return { version: 1,
      favorites: [...new Set(stored.favorites.map(id => id === 'burp-community' ? 'yakit' : id))],
      recent: [...new Set(stored.recent.filter((id): id is NativeToolId => id !== 'burp-community'))],
    }
  }

  private async missing(root: string, tool: InstalledNativeTool, entry = tool.entry): Promise<string[]> {
    const missing: string[] = []
    for (const path of new Set([entry.path, entry.cwd, ...tool.roots, ...entry.runtime ? [entry.runtime] : [],
      ...entry.dotnetRoot ? [entry.dotnetRoot] : [], ...entry.pythonRoot ? [entry.pythonRoot] : [], ...entry.requiredFiles ?? []])) {
      try {
        const resolved = await resolveNativePath(root, path)
        const info = await stat(resolved)
        const fileRequired = path === entry.path || path === entry.runtime || entry.requiredFiles?.includes(path)
        const directoryRequired = path === entry.cwd || path === entry.dotnetRoot || path === entry.pythonRoot || tool.roots.includes(path)
        if (fileRequired && !info.isFile() || directoryRequired && !info.isDirectory()) missing.push(path)
      } catch (error) {
        if (error instanceof Error) missing.push(path)
        else throw error
      }
    }
    return missing
  }

  /**
   * Read current tool availability and durable preferences.
   * @returns tool summaries; availability does not imply a completed functional acceptance run.
   */
  async listTools(): Promise<NativeToolCatalog> {
    const [{ tools, verified, catalogOutdated, updateBytes }, preferences] = await Promise.all([this.catalog(), this.preferences()])
    return {
      tools: await Promise.all(tools.map(async (tool) => {
        const summary = { id: tool.id, name: tool.name, category: tool.category, version: tool.version,
          launchKind: tool.entry.kind === 'console' ? 'terminal' as const : tool.entry.kind === 'web' ? 'web' as const : 'desktop' as const,
          verified: verified.has(tool.id), outdated: tool.state?.outdated ?? false, downloadBytes: tool.state?.downloadBytes ?? 0 }
        if (tool.state?.installed === false) {
          return { ...summary, status: 'available' as const, missing: [],
            ...tool.variants ? { variants: tool.variants.map(variant => ({ id: variant.id, name: variant.name, status: 'available' as const })) } : {} }
        }
        const missing = await this.missing(tool.installRoot, tool)
        return { ...summary, status: missing.length ? 'missing' as const : 'ready' as const, missing,
          ...tool.variants ? { variants: await Promise.all(tool.variants.map(async variant => ({ id: variant.id, name: variant.name,
            status: (await this.missing(tool.installRoot, tool, variant.entry)).length ? 'missing' as const : 'ready' as const }))) } : {},
        }
      })), preferences: { favorites: preferences.favorites, recent: preferences.recent }, catalogOutdated, updateBytes,
    }
  }

  private change(update: (current: CurrentPreferences) => void): Promise<void> {
    const operation = this.mutation.then(async () => {
      const current = await this.preferences()
      update(current)
      await mkdir(dirname(this.preferencesPath), { recursive: true })
      const temporary = `${this.preferencesPath}.${randomUUID()}.tmp`
      try {
        await writeFile(temporary, `${JSON.stringify(current, null, 2)}\n`, { flag: 'wx' })
        await renameToolPackPath(temporary, this.preferencesPath)
      } finally { await rm(temporary, { force: true }) }
    })
    this.mutation = operation.catch(() => { /* Each caller receives its own write error; later edits remain possible. */ })
    return operation
  }

  /**
   * Save the user's ordered favorites without losing concurrent recent launches.
   * @param favorites - validated tool identities.
   */
  setFavorites(favorites: readonly NativeToolId[]): Promise<void> {
    return this.change((current) => { current.favorites = [...new Set(favorites)] })
  }

  /**
   * Finish accepted launches and preference writes before the owning carrier exits.
   * @returns completion after the current operation queue has settled.
   */
  async waitForIdle(): Promise<void> {
    await Promise.all([...this.pending.values()])
    await this.mutation
  }

  /**
   * Start one installed tool; overlapping requests share the same launch operation.
   * @param id - catalog tool identity.
   * @param variant - x32dbg selection for the debugger entry.
   * @returns whether the native launcher accepted the request, or a user-readable failure.
   */
  launchTool(id: NativeToolId, variant?: 'x32'): Promise<NativeToolLaunchResult> {
    const key = `${id}:${variant ?? ''}`
    const existing = this.pending.get(key)
    if (existing) return existing
    const operation = this.launch(id, variant)
      .catch((error: unknown) => ({ ok: false, error: error instanceof Error ? error.message : String(error) }))
      .finally(() => { this.pending.delete(key) })
    this.pending.set(key, operation)
    return operation
  }

  private async launch(id: NativeToolId, variant?: 'x32'): Promise<NativeToolLaunchResult> {
    const { tools } = await this.catalog()
    const tool = tools.find(value => value.id === id)
    if (tool === undefined) throw new Error('工具不在已安装目录中')
    if (tool.state?.installed === false) throw new Error('工具尚未下载，请先在工具列表中下载')
    const root = tool.installRoot
    const entry = variant === undefined ? tool.entry : tool.variants?.at(0)?.entry
    if (entry === undefined) throw new Error('工具启动选项不可用')
    const missing = await this.missing(root, tool, entry)
    if (missing.length) throw new Error(`工具文件缺失，请修复工具包：${missing.join('、')}`)
    const target = await resolveNativePath(root, entry.path)
    const executable = entry.runtime === undefined ? target : await resolveNativePath(root, entry.runtime)
    await this.options.start({ id, name: tool.name, kind: entry.kind, target, executable,
      userData: resolve(this.options.userData, 'native-tools', id),
      cwd: await resolveNativePath(root, entry.cwd),
      args: entry.kind === 'java' ? ['-jar', target, ...entry.args] : entry.args,
      roots: await Promise.all(tool.roots.map(path => resolveNativePath(root, path))),
      ...entry.dotnetRoot ? { dotnetRoot: await resolveNativePath(root, entry.dotnetRoot) } : {},
      ...entry.pythonRoot ? { pythonRoot: await resolveNativePath(root, entry.pythonRoot) } : {},
    })
    try { await this.change((current) => { current.recent = [id, ...current.recent.filter(value => value !== id)].slice(0, ids.length) }) }
    catch (error) {
      return { ok: true, warning: `工具已打开，但最近使用记录未保存：${error instanceof Error ? error.message : String(error)}` }
    }
    return { ok: true }
  }
}
