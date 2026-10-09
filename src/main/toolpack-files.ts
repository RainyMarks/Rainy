/** Filesystem checks and durable records used by the native-tool installer. */
import { createHash, randomUUID } from 'node:crypto'
import type { Stats } from 'node:fs'
import { dirname, join, parse, relative, resolve, sep } from 'node:path'
import { ToolPackInstallError, isToolPackPath } from './toolpack-format.ts'
import { toolPackFileSystem } from './toolpack-fs.ts'

const { createReadStream } = toolPackFileSystem
const { copyFile, lstat, mkdir, open, readdir, readFile, rename, rm } = toolPackFileSystem.promises

/** Resolve a checked relative package path below one absolute root.
 * @param root Installation or staging directory.
 * @param path Slash-separated package path.
 * @returns Absolute child path.
 */
export function toolPackChild(root: string, path: string): string {
  if (!isToolPackPath(path)) throw new ToolPackInstallError('unsafe-path', `工具包路径无效：${path}`)
  return resolve(root, ...path.split('/'))
}

/** Stop before another interruptible installation step.
 * @param signal Optional caller cancellation signal.
 * @returns Normally unless cancellation was requested.
 */
export function checkToolPackCancellation(signal?: AbortSignal): void {
  if (signal?.aborted) throw new ToolPackInstallError('cancelled', '工具安装已取消。已校验的暂存文件会在重试时复用。')
}

/** Read file metadata without following links; absence is distinct from other filesystem failures.
 * @param path Absolute file or directory path.
 * @returns The metadata, or undefined when the path is absent.
 */
export async function toolPackStat(path: string): Promise<Stats | undefined> {
  try { return await lstat(path) } catch (error) {
    if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return undefined
    throw error
  }
}

/** Reject links or nondirectory ancestors on every existing component of an absolute path.
 * @param path Absolute target, which may not exist yet.
 * @returns Completion when the existing path components are ordinary files or directories.
 */
export async function assertToolPackPath(path: string): Promise<void> {
  const absolute = resolve(path)
  const parsed = parse(absolute)
  let cursor = parsed.root
  const parts = absolute.slice(parsed.root.length).split(sep).filter(Boolean)
  for (const [index, part] of parts.entries()) {
    cursor = join(cursor, part)
    const entry = await toolPackStat(cursor)
    if (!entry) return
    if (entry.isSymbolicLink() || entry.isFile() && entry.nlink > 1) throw new ToolPackInstallError('link-not-allowed', `安装路径包含链接或重解析目录：${cursor}`)
    if (index < parts.length - 1 && !entry.isDirectory()) throw new ToolPackInstallError('unsafe-path', `安装路径的父级不是目录：${cursor}`)
    if (!entry.isDirectory() && !entry.isFile()) throw new ToolPackInstallError('unsafe-path', `安装路径包含不支持的文件类型：${cursor}`)
  }
}

/** Enumerate ordinary files without traversing any link.
 * @param root File or directory to inspect.
 * @param signal Optional cancellation signal.
 * @returns Absolute files and their sizes.
 */
export async function toolPackTree(root: string, signal?: AbortSignal): Promise<Array<{ path: string; bytes: number }>> {
  await assertToolPackPath(root)
  const files: Array<{ path: string; bytes: number }> = []
  async function visit(path: string): Promise<void> {
    checkToolPackCancellation(signal)
    const entry = await toolPackStat(path)
    if (!entry) return
    if (entry.isSymbolicLink() || entry.isFile() && entry.nlink > 1) throw new ToolPackInstallError('link-not-allowed', `工具目录包含链接，已保留原文件：${path}`)
    if (entry.isDirectory()) {
      for (const name of await readdir(path)) await visit(join(path, name))
    } else if (entry.isFile()) files.push({ path, bytes: entry.size })
    else throw new ToolPackInstallError('unsafe-path', `工具目录包含不支持的文件类型：${path}`)
  }
  await visit(root)
  return files
}

/** Hash a regular file while honoring cancellation between reads.
 * @param path Absolute file path.
 * @param signal Optional cancellation signal.
 * @returns Lowercase SHA-256 digest.
 */
export async function toolPackHash(path: string, signal?: AbortSignal): Promise<string> {
  await assertToolPackPath(path)
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(path)) {
    checkToolPackCancellation(signal)
    if (!Buffer.isBuffer(chunk)) throw new ToolPackInstallError('file-integrity', `文件读取返回了非二进制数据：${path}`)
    hash.update(chunk)
  }
  return hash.digest('hex')
}

const sharingViolationCodes = new Set(['EPERM', 'EACCES', 'EBUSY'])

/** Rename a file or directory, retrying for about two seconds while another Windows handle holds the source or replaced target open.
 * @param source Existing path.
 * @param destination Target path; an existing file is replaced.
 * @returns Completion after the rename; other platforms and persistent failures reject with the operating-system error.
 */
export async function renameToolPackPath(source: string, destination: string): Promise<void> {
  for (let attempt = 1; ; attempt++) {
    try { await rename(source, destination); return } catch (error) {
      if (process.platform !== 'win32' || attempt === 10
        || !(error instanceof Error && 'code' in error && typeof error.code === 'string' && sharingViolationCodes.has(error.code))) throw error
    }
    await new Promise(resolve => setTimeout(resolve, attempt * 50))
  }
}

/** Atomically replace a file only after its new bytes reach the filesystem; a failed write removes its temporary file.
 * @param path Absolute file path.
 * @param text Complete file content.
 * @returns Completion after the atomic replacement.
 */
export async function writeToolPackFile(path: string, text: string): Promise<void> {
  await assertToolPackPath(path)
  await mkdir(dirname(path), { recursive: true })
  const temporary = `${path}.${randomUUID()}.tmp`
  try {
    const file = await open(temporary, 'wx', 0o600)
    try {
      await file.writeFile(text)
      await file.sync()
    } finally { await file.close() }
    await renameToolPackPath(temporary, path)
  } finally { await rm(temporary, { force: true }) }
}

/** Atomically replace a JSON record only after its new bytes reach the filesystem.
 * @param path Absolute record path.
 * @param value JSON-serializable object.
 * @returns Completion after the atomic replacement.
 */
export async function writeToolPackRecord(path: string, value: object): Promise<void> {
  await writeToolPackFile(path, JSON.stringify(value, null, 2) + '\n')
}

/** Read optional local JSON without hiding malformed or inaccessible records.
 * @param path Absolute record path.
 * @returns Parsed JSON, or undefined when the record is absent.
 */
export async function readToolPackRecord(path: string): Promise<unknown> {
  await assertToolPackPath(path)
  if (!await toolPackStat(path)) return undefined
  try { return JSON.parse(await readFile(path, 'utf8')) } catch (error) { throw new ToolPackInstallError('invalid-record', `工具安装记录无法读取，已保留原文件：${path}`, { cause: error }) }
}

/** Copy a declared user-data path into the staged replacement without following links.
 * @param source Existing file or directory.
 * @param destination Matching path inside the staged tool directory.
 * @param signal Optional cancellation signal.
 * @returns Total copied bytes.
 */
export async function copyToolPackUserData(source: string, destination: string, signal?: AbortSignal): Promise<number> {
  await assertToolPackPath(source)
  await assertToolPackPath(destination)
  const sourceStat = await toolPackStat(source)
  if (!sourceStat) return 0
  const files = await toolPackTree(source, signal)
  if (sourceStat.isDirectory()) await mkdir(destination, { recursive: true })
  let bytes = 0
  for (const file of files) {
    checkToolPackCancellation(signal)
    const target = sourceStat.isFile() ? destination : resolve(destination, relative(source, file.path))
    await assertToolPackPath(target)
    await mkdir(dirname(target), { recursive: true })
    await copyFile(file.path, target)
    bytes += file.bytes
  }
  return bytes
}

/** Remove only the staged copy of a declared configuration path before refreshing it.
 * @param stageRoot Owned staging directory for the current pack ID.
 * @param path Declared installation-relative preservation path.
 * @returns Completion after the old staged copy has been removed.
 */
export async function resetToolPackStagedUserData(stageRoot: string, path: string): Promise<void> {
  const target = toolPackChild(stageRoot, path)
  await assertToolPackPath(target)
  const entry = await toolPackStat(target)
  if (!entry) return
  await toolPackTree(target)
  await rm(target, { recursive: entry.isDirectory(), force: false })
}
