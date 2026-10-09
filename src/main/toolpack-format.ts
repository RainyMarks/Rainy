/** Validated metadata for the offline native-tool archive and installation units. */
import { createHash } from 'node:crypto'
import { z } from 'zod'

const hashSchema = z.string().regex(/^[a-f0-9]{64}$/)
const bytesSchema = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER)

/** Reject filesystem aliases and platform-specific path escapes before touching disk.
 * @param path Slash-separated installation-relative path.
 * @returns Whether every component is an ordinary Windows filename.
 */
export function isToolPackPath(path: string): boolean {
  return path.length > 0 && !/[\\:\x00-\x1f\x7f<>"|?*]/.test(path) && path.split('/').every(part =>
    part.length > 0 && part !== '.' && part !== '..' && !/[. ]$/.test(part)
    && !/^(con|prn|aux|nul|clock\$|conin\$|conout\$|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)/i.test(part))
}

const pathSchema = z.string().refine(isToolPackPath, 'Unsafe installation-relative path')
const fileSchema = z.object({ path: pathSchema, bytes: bytesSchema, sha256: hashSchema }).strict()
const unitSchema = z.object({ path: pathSchema, kind: z.enum(['directory', 'file']), preserve: z.array(pathSchema) }).strict()
const metadataFields = { id: hashSchema, format: z.literal('tar.gz'), unpackedBytes: bytesSchema,
  files: z.array(fileSchema).min(1), units: z.array(unitSchema).min(1) }

/** Name of a unit archive; the digest identifies the unit's file inventory, so an unchanged tool keeps its archive. */
export const TOOL_UNIT_ARCHIVE = /^rainy-unit-[a-f0-9]{20}\.tar\.gz$/

/** Whether a package file belongs to an installation unit.
 * @param unit Directory or catalog-file unit.
 * @param path Package file path.
 * @returns Membership.
 */
export function unitContains(unit: { readonly path: string; readonly kind: 'directory' | 'file' }, path: string): boolean {
  return unit.kind === 'file' ? path === unit.path : path.startsWith(unit.path + '/')
}

/** Metadata names one archive split into volumes (version 1) or one archive per installation unit (version 2). */
export const toolPackMetadataSchema = z.discriminatedUnion('version', [
  z.object({ version: z.literal(1), ...metadataFields, volumeSize: z.number().int().positive().max(2 * 1024 ** 3),
    volumes: z.array(z.object({ file: pathSchema.refine(path => !path.includes('/')), bytes: bytesSchema.refine(bytes => bytes > 0), sha256: hashSchema }).strict()).min(1),
  }).strict(),
  z.object({ version: z.literal(2), ...metadataFields,
    archives: z.array(z.object({ unit: pathSchema, file: z.string().regex(TOOL_UNIT_ARCHIVE),
      bytes: bytesSchema.refine(bytes => bytes > 0), sha256: hashSchema }).strict()).min(1),
  }).strict(),
]).superRefine((metadata, context) => {
  const reject = (message: string): void => { context.addIssue({ code: 'custom', message }) }
  const paths = new Set<string>()
  const aliases = new Map<string, string>()
  for (const file of metadata.files) {
    const key = file.path.toLowerCase()
    if (paths.has(key)) reject(`Duplicate file: ${file.path}`)
    paths.add(key)
    const parts = file.path.split('/')
    for (let index = 1; index <= parts.length; index++) {
      const original = parts.slice(0, index).join('/')
      const previous = aliases.get(original.toLowerCase())
      if (previous && previous !== original) reject(`Inconsistent Windows path spelling: ${original}`)
      aliases.set(original.toLowerCase(), original)
    }
    if (!metadata.units.some(unit => unitContains(unit, file.path))) reject(`File outside install units: ${file.path}`)
  }
  if (metadata.files.reduce((sum, file) => sum + file.bytes, 0) !== metadata.unpackedBytes) reject('Unpacked byte count does not match file inventory')
  const units = new Set<string>()
  for (const unit of metadata.units) {
    if (units.has(unit.path.toLowerCase())) reject(`Duplicate install unit: ${unit.path}`)
    units.add(unit.path.toLowerCase())
    if (unit.kind === 'directory' && !/^(tools\/[^/]+|runtime\/windows\/[^/]+)$/.test(unit.path)) reject(`Invalid tool directory: ${unit.path}`)
    if (unit.kind === 'file' && !['tools/manifest.json', 'tools/verified.json'].includes(unit.path)) reject(`Invalid catalog file: ${unit.path}`)
    for (const path of unit.preserve) if (unit.kind !== 'directory' || !path.startsWith(unit.path + '/')) reject(`Preserved path outside tool directory: ${path}`)
    const preserved = unit.preserve.map(path => path.toLowerCase())
    const preservedPaths = new Set(preserved)
    if (preservedPaths.size !== preserved.length) reject(`Duplicate preserved paths in ${unit.path}`)
    for (const path of preserved) {
      const parts = path.split('/')
      for (let index = 1; index < parts.length; index++) {
        if (preservedPaths.has(parts.slice(0, index).join('/'))) reject(`Overlapping preserved paths: ${path}`)
      }
    }
    for (const path of unit.preserve) {
      const parts = path.split('/')
      for (let index = 1; index <= parts.length; index++) {
        const original = parts.slice(0, index).join('/')
        const existing = aliases.get(original.toLowerCase())
        if (existing && existing !== original) reject(`Inconsistent preserved path spelling: ${original}`)
      }
    }
    if (!metadata.files.some(file => unitContains(unit, file.path))) reject(`Empty installation unit: ${unit.path}`)
  }
  if (!metadata.files.some(file => file.path === 'tools/manifest.json')) reject('Tool catalog is missing')
  const expectedId = createHash('sha256').update(JSON.stringify({ files: metadata.files, units: metadata.units })).digest('hex')
  if (expectedId !== metadata.id) reject('Pack ID does not match the file and installation-unit records')
  if (metadata.version === 1) {
    const volumeNames = new Set<string>()
    for (const [index, volume] of metadata.volumes.entries()) {
      const expectedName = `native-tools-${metadata.id.slice(0, 16)}.tar.gz.${String(index + 1).padStart(3, '0')}`
      if (volume.file !== expectedName || volumeNames.has(volume.file.toLowerCase())) reject(`Unexpected archive volume: ${volume.file}`)
      volumeNames.add(volume.file.toLowerCase())
      if (volume.bytes > metadata.volumeSize || index < metadata.volumes.length - 1 && volume.bytes !== metadata.volumeSize) reject(`Invalid volume size: ${volume.file}`)
    }
    return
  }
  const archived = new Set<string>()
  const archiveNames = new Set<string>()
  for (const archive of metadata.archives) {
    if (!metadata.units.some(unit => unit.path === archive.unit) || archived.has(archive.unit)) reject(`Unexpected unit archive: ${archive.unit}`)
    if (archiveNames.has(archive.file)) reject(`Duplicate unit archive: ${archive.file}`)
    archived.add(archive.unit)
    archiveNames.add(archive.file)
  }
  if (archived.size !== metadata.units.length) reject('Every installation unit requires one archive')
})

/** Trusted fields after metadata parsing succeeds. */
export type ToolPackMetadata = z.infer<typeof toolPackMetadataSchema>

/** Single split archive inventory. */
export type ToolPackMetadataV1 = Extract<ToolPackMetadata, { version: 1 }>

/** Per-unit archive inventory. */
export type ToolPackMetadataV2 = Extract<ToolPackMetadata, { version: 2 }>

/** One directory or catalog file replaced atomically during the installation transaction. */
export type ToolPackUnit = ToolPackMetadata['units'][number]

/** Installed units of one pack; version 1 records predate partial installation and cover every unit. */
export const installedToolPackSchema = z.discriminatedUnion('version', [
  z.object({ version: z.literal(1), packId: hashSchema }).strict(),
  z.object({ version: z.literal(2), packId: hashSchema, units: z.array(pathSchema) }).strict(),
])

/** Validated installed-pack record. */
export type InstalledToolPack = z.infer<typeof installedToolPackSchema>

/** Installer progress suitable for the maintenance window or setup log. */
export interface ToolPackProgress {
  phase: 'checking-media' | 'checking-space' | 'extracting' | 'verifying' | 'preserving' | 'switching' | 'rolling-back' | 'complete'
  message: string
  completedBytes: number
  totalBytes: number
  currentPath?: string
}

/** Installation input. All paths are chosen by the main process, not renderer content. */
export interface InstallNativeToolPackOptions {
  installRoot: string
  mediaDirectory: string
  metadataPath: string
  /** Units installed after the transaction; omitted installs every unit. Installed units left out are retired. */
  units?: readonly string[]
  /** Units replaced even when their recorded files are unchanged, for example after a failed integrity check. */
  replace?: readonly string[]
  /** The caller already holds this directory's installation lock. */
  lockHeld?: boolean
  onProgress?(progress: ToolPackProgress): void
  signal?: AbortSignal
}

/** Successful installation including retained recovery files. */
export interface NativeToolPackInstallResult {
  status: 'installed'
  packId: string
  installedFiles: number
  reusedFiles: number
  backupDirectory: string
  /** Program-file bytes removed from backups after the commit; user files stay there. */
  prunedBytes: number
}

/** Platform calls that own free-space observations, busy-process checks, and atomic renames. */
export interface ToolPackPlatform {
  availableBytes(path: string): Promise<number>
  assertNotBusy(installRoot: string, units: ToolPackUnit[]): Promise<void>
  move(source: string, destination: string): Promise<void>
}

/** Actionable installation failure; cancellation preserves verified staging files. */
export class ToolPackInstallError extends Error {
  readonly code: string
  constructor(code: string, message: string, options?: ErrorOptions) { super(message, options); this.name = 'ToolPackInstallError'; this.code = code }
}
