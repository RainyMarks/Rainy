/** Verified offline component import into content-addressed application-owned directories. */
import { createHash, randomUUID } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { mkdir, open, readFile, rename, stat, statfs, lstat, readdir, rm } from 'node:fs/promises'
import { isAbsolute, join, relative, resolve, sep } from 'node:path'
import { x as extractTar } from 'tar'
import { z } from 'zod'
import { brandString } from '../shared/brand.ts'
import type { Branded } from '../shared/brand.ts'

/** One release-approved runtime component family and platform. */
export type EnvironmentComponentId = Branded<'RainyEnvironmentComponentId'>

/** Release-owned component descriptor; hashes are anchored in the installed release catalog. */
export interface EnvironmentComponent {
  version: 1
  id: EnvironmentComponentId
  platform: 'windows' | 'linux'
  architecture: 'x64'
  file: string
  bytes: number
  sha256: string
  unpackedBytes: number
  manifestSha256: string
}
const digestSchema = z.string().regex(/^[a-f0-9]{64}$/u)
/** Validate release metadata before any extraction or path construction. */
export const environmentComponentSchema: z.ZodType<EnvironmentComponent> = z.object({
  version: z.literal(1), id: z.string().regex(/^(?:windows|linux)-(?:basic|science-cpu|science-cuda|cpp|development)$/u)
    .transform(value => brandString<EnvironmentComponentId>(value)),
  platform: z.enum(['windows', 'linux']), architecture: z.literal('x64'), file: z.string(),
  bytes: z.number().int().positive(), sha256: digestSchema, unpackedBytes: z.number().int().positive(), manifestSha256: digestSchema,
}).strict().refine(value => value.file === `${value.id}.tar.gz` && value.id.startsWith(value.platform + '-'), 'Component identity and archive filename differ.')
const fileSchema = z.object({ path: z.string().min(1), bytes: z.number().int().nonnegative(), sha256: digestSchema }).strict()
const manifestSchema = z.object({ version: z.literal(1), id: z.string(), platform: z.literal('windows'), architecture: z.literal('x64'),
  files: z.array(fileSchema), unpackedBytes: z.number().int().nonnegative(), python: z.string().nullable() }).strict()

/** @param path - regular file to hash. @returns its SHA-256 digest. */
export async function componentDigest(path: string): Promise<string> {
  const digest = createHash('sha256')
  const source: AsyncIterable<Buffer> = createReadStream(path)
  for await (const chunk of source) digest.update(chunk)
  return digest.digest('hex')
}
function childPath(root: string, path: string): string {
  if (!path || path.includes('\\') || path.includes(':') || path.split('/').some(part => !part || part === '.' || part === '..')) throw new Error('The component contains an invalid relative path.')
  const target = resolve(root, ...path.split('/'))
  const child = relative(root, target)
  if (!child || isAbsolute(child) || child === '..' || child.startsWith(`..${sep}`)) throw new Error('The component path escapes its installation directory.')
  return target
}

/** @param path - installed release catalog. @returns the full release-approved component set. */
export async function readEnvironmentComponentCatalog(path: string): Promise<EnvironmentComponent[]> {
  return z.object({ version: z.literal(1), components: z.array(environmentComponentSchema) }).strict()
    .parse(JSON.parse(await readFile(path, 'utf8'))).components
}

/**
 * @param directory - unpacked component.
 * @param expected - release descriptor.
 * @param destination - final native loading directory.
 * @returns successful inventory verification.
 */
export async function verifyWindowsComponent(directory: string, expected: EnvironmentComponent, destination = directory): Promise<void> {
  const manifestPath = join(directory, 'component.json')
  if (await componentDigest(manifestPath) !== expected.manifestSha256) throw new Error('The component manifest does not match this release.')
  const manifest = manifestSchema.parse(JSON.parse(await readFile(manifestPath, 'utf8')))
  if (manifest.id !== expected.id || manifest.unpackedBytes !== expected.unpackedBytes) throw new Error('The component manifest identity differs.')
  const known = new Set(['component.json'])
  for (const file of manifest.files) {
    if (known.has(file.path)) throw new Error('The component inventory contains a duplicate path.')
    known.add(file.path)
    if (/\.(?:dll|pyd|exe)$/iu.test(file.path) && childPath(destination, file.path).length > 240) throw new Error('The component path is too long for Windows native-library loading; use a shorter application data directory.')
    const path = childPath(directory, file.path)
    const info = await lstat(path)
    if (!info.isFile() || info.isSymbolicLink() || info.size !== file.bytes || await componentDigest(path) !== file.sha256) throw new Error(`Component verification failed: ${file.path}`)
  }
  const visit = async (root: string): Promise<void> => {
    for (const entry of await readdir(root, { withFileTypes: true })) {
      const path = join(root, entry.name)
      if (entry.isDirectory()) await visit(path)
      else if (!entry.isFile() || !known.has(relative(directory, path).split(sep).join('/'))) throw new Error('The component contains an unlisted entry.')
    }
  }
  await visit(directory)
}

/**
 * Import a release-approved Windows component without changing an existing user environment.
 * @param options - archive directory, release descriptor and application-owned component root.
 * @returns verified installation directory; previous generations remain available.
 */
export async function installWindowsComponent(options: {
  mediaDirectory: string
  root: string
  component: EnvironmentComponent
  progress?: (message: string) => void
}): Promise<string> {
  const expected = environmentComponentSchema.parse(options.component)
  if (expected.platform !== 'windows') throw new Error('This component belongs to another execution target.')
  const root = resolve(options.root)
  const archive = join(resolve(options.mediaDirectory), expected.file)
  if ((await stat(archive)).size !== expected.bytes || await componentDigest(archive) !== expected.sha256) throw new Error('The offline component archive is missing or corrupt.')
  await mkdir(root, { recursive: true, mode: 0o700 })
  const destination = join(root, expected.id, expected.sha256.slice(0, 32))
  const emit = (message: string): void => { try { options.progress?.(message) } catch (error) { console.error('Component progress callback failed:', error) } }
  let exists = false
  try { await stat(destination); exists = true } catch (error) { if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error }
  if (exists) await verifyWindowsComponent(destination, expected)
  else {
    const disk = await statfs(root, { bigint: true })
    const required = BigInt(expected.unpackedBytes) + BigInt(Math.max(32 * 1024 * 1024, Math.ceil(expected.unpackedBytes / 20)))
    if (disk.bavail * disk.bsize < required) throw new Error('The component destination does not have enough free space for verified extraction.')
    const staging = join(root, `.component-${randomUUID()}`)
    await mkdir(staging, { mode: 0o700 })
    try {
      emit('正在解压已验证的离线组件…')
      await extractTar({ file: archive, cwd: staging, strict: true, preservePaths: false,
        filter: (path, entry) => {
          childPath(staging, path.replace(/\/$/u, ''))
          if ('type' in entry ? !['File', 'Directory'].includes(entry.type) : !entry.isFile() && !entry.isDirectory()) throw new Error('Windows components cannot contain links or special files.')
          return true
        } })
      emit('正在核对组件文件…')
      await verifyWindowsComponent(staging, expected, destination)
      await mkdir(dirnameOf(destination), { recursive: true })
      await rename(staging, destination)
    } finally {
      const child = relative(root, staging)
      if (!child || child.startsWith('..') || isAbsolute(child)) throw new Error('Component cleanup escaped its owned root.')
      await rm(staging, { recursive: true, force: true })
    }
  }
  const activePath = join(root, 'active.json')
  let active: Record<string, string> = {}
  try { active = z.record(z.string(), z.string()).parse(JSON.parse(await readFile(activePath, 'utf8'))) }
  catch (error) { if (!(error instanceof Error && 'code' in error && error.code === 'ENOENT')) throw error }
  active[expected.id] = relative(root, destination).split(sep).join('/')
  const pending = join(root, `.active-${randomUUID()}.json`)
  const file = await open(pending, 'wx', 0o600)
  try { await file.writeFile(JSON.stringify(active, null, 2) + '\n'); await file.sync() } finally { await file.close() }
  await rename(pending, activePath)
  return destination
}

function dirnameOf(path: string): string { return resolve(path, '..') }
