/** Atomic replacement of application-owned local records. */
import { randomUUID } from 'node:crypto'
import { lstat, mkdir, open, rename, unlink } from 'node:fs/promises'
import { dirname, join } from 'node:path'

function missing(error: unknown): boolean {
  return error instanceof Error && 'code' in error && error.code === 'ENOENT'
}

/**
 * Atomically replace a record without following an existing symbolic link.
 * @param path - application-owned record path.
 * @param bytes - complete serialized record.
 * @returns after the flushed replacement has been published.
 */
export async function writePrivateRecord(path: string, bytes: Buffer): Promise<void> {
  await mkdir(dirname(path), { recursive: true, mode: 0o700 })
  try {
    const info = await lstat(path)
    if (!info.isFile() || info.isSymbolicLink()) throw new Error('本地记录路径不是普通文件')
  } catch (error) { if (!missing(error)) throw error }
  const temporary = join(dirname(path), `.rainy-${randomUUID()}.tmp`)
  const file = await open(temporary, 'wx', 0o600)
  let published = false
  try {
    try { await file.writeFile(bytes); await file.sync() }
    finally { await file.close() }
    await rename(temporary, path)
    published = true
  } finally {
    if (!published) {
      try { await unlink(temporary) }
      catch (error) { if (!missing(error)) throw error }
    }
  }
}
