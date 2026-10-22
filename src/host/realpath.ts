/** Canonical paths that also work on Windows drives the mount manager does not know. */
import { realpath as walkRealpath } from 'node:fs'
import { realpath as nativeRealpath, stat } from 'node:fs/promises'
import { resolve } from 'node:path'
import { promisify } from 'node:util'

const walk = promisify(walkRealpath)

/**
 * Resolve links like `fs.promises.realpath`. On Windows that asks GetFinalPathNameByHandle for the volume's drive letter,
 * which fails on volumes the mount manager does not know (forensic image mounters such as 火眼, RAM disks, some virtual
 * drives) even though the path is there: the folder then read as "not a folder". When the path exists, Node's JavaScript
 * walk (lstat and readlink per component) answers instead, and the plain resolved path as a last resort.
 * @param path Path to canonicalize.
 * @param platform Platform whose filesystem is asked; tests pass `win32`.
 * @returns The canonical path.
 */
export async function realpath(path: string, platform: NodeJS.Platform = process.platform): Promise<string> {
  try { return await nativeRealpath(path) } catch (error) {
    if (platform !== 'win32') throw error
    // A missing path, or one below a file, keeps the native error: callers walk up on ENOENT and ENOTDIR.
    try { await stat(path) } catch (_missing) { throw error }
    try { return await walk(path) } catch (_unresolvable) { return resolve(path) }
  }
}
