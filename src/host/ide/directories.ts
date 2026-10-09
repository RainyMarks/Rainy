/** The in-app directory browser: one level of host directories outside any project. */
import { access, mkdir, opendir, stat } from 'node:fs/promises'
import { homedir } from 'node:os'
import { basename, dirname, join, posix, resolve, win32 } from 'node:path'
import type { IdeDirectoryListing } from '../../shared/ide-files-protocol.ts'
import { IdeOperationError } from './files-core.ts'

type Entry = IdeDirectoryListing['entries'][number]

/**
 * Whether a path names one fixed location: POSIX-absolute, or on Windows drive-qualified (`C:\…`) or complete UNC.
 * @param path Candidate path.
 * @returns Whether the path does not depend on the process's current directory or drive.
 */
export function fullyQualified(path: string): boolean {
  return process.platform === 'win32'
    ? win32.isAbsolute(path) && /^(?:[A-Za-z]:[\\/]|[\\/]{2}[^\\/]+[\\/]+[^\\/]+)/u.test(path)
    : posix.isAbsolute(path) && !path.includes('\0')
}

function absolute(path: string): string {
  if (!fullyQualified(path) || path.includes('\0')) throw new IdeOperationError('invalid-path', 'Choose an absolute directory path.')
  return resolve(path)
}

/** Drive probes still running; an unreachable network drive can hold a filesystem thread for a long time. */
const driveProbes = new Map<string, Promise<boolean>>()
const DRIVE_PROBE_MS = 1000

function driveExists(drive: string): Promise<boolean> {
  let probe = driveProbes.get(drive)
  if (probe === undefined) {
    probe = access(drive).then(() => true, () => false)
    driveProbes.set(drive, probe)
    void probe.then(() => { driveProbes.delete(drive) })
  }
  // A drive letter that does not answer in time is mapped but slow, so it is offered.
  return Promise.race([probe, new Promise<boolean>((resolve) => { setTimeout(() => { resolve(true) }, DRIVE_PROBE_MS).unref() })])
}

async function roots(): Promise<string[]> {
  if (process.platform !== 'win32') return [...new Set(['/', homedir()])]
  const letters = Array.from({ length: 26 }, (_, index) => `${String.fromCharCode(65 + index)}:\\`)
  const present = await Promise.all(letters.map(async drive => (await driveExists(drive)) ? drive : undefined))
  return present.filter((drive): drive is string => drive !== undefined)
}

function failure(error: unknown, path: string): IdeOperationError {
  if (error instanceof IdeOperationError) return error
  const code = error !== null && typeof error === 'object' && 'code' in error ? error.code : undefined
  if (code === 'ENOENT') return new IdeOperationError('not-found', `The directory does not exist: ${path}`)
  if (code === 'ENOTDIR') return new IdeOperationError('not-directory', `The path is not a directory: ${path}`)
  if (code === 'EACCES' || code === 'EPERM') return new IdeOperationError('permission-denied', `The directory cannot be read: ${path}`)
  return new IdeOperationError('io-error', error instanceof Error ? error.message : `The directory cannot be read: ${path}`)
}

/**
 * List the child directories of one directory, following links to directories.
 * Entries are sorted by name; at most `limit` names are kept (the first ones by name).
 * @param path Absolute directory; omitted means the home directory.
 * @param showHidden Whether dot-named directories are included.
 * @param limit Largest number of entries returned.
 * @param signal Cancels the scan.
 * @returns The listing with its parent and the filesystem roots.
 */
export async function listDirectories(path: string | undefined, showHidden: boolean, limit: number, signal?: AbortSignal): Promise<IdeDirectoryListing> {
  const target = path === undefined ? homedir() : absolute(path)
  const names: string[] = []
  try {
    const directory = await opendir(target)
    try {
      for (;;) {
        signal?.throwIfAborted()
        const entry = await directory.read()
        if (entry === null) break
        if (!showHidden && entry.name.startsWith('.')) continue
        if (entry.isDirectory()) names.push(entry.name)
        else if (entry.isSymbolicLink()) {
          try {
            if ((await stat(join(target, entry.name))).isDirectory()) names.push(entry.name)
          } catch (_brokenLink) { /* A broken or cyclic link cannot be entered and is not listed. */ }
        }
        if (names.length > limit * 2) {
          names.sort((a, b) => a.localeCompare(b))
          names.length = limit
        }
      }
    } finally {
      await directory.close()
    }
  } catch (error) {
    signal?.throwIfAborted()
    throw failure(error, target)
  }
  const entries: Entry[] = names.sort((a, b) => a.localeCompare(b)).slice(0, limit)
    .map(name => ({ name, path: join(target, name), hidden: name.startsWith('.') }))
  const parent = dirname(target)
  return { path: target, parent: parent === target ? null : parent, roots: await roots(), entries }
}

/**
 * Create one directory whose parent exists, then list it.
 * @param path Absolute path of the new directory.
 * @param limit Largest number of entries in the returned listing.
 * @returns The listing of the new directory.
 */
export async function createDirectory(path: string, limit: number): Promise<IdeDirectoryListing> {
  const target = absolute(path)
  const name = basename(target)
  if (dirname(target) === target || name === '' || name === '.' || name === '..') {
    throw new IdeOperationError('invalid-path', 'Choose a new directory inside an existing directory.')
  }
  try {
    await mkdir(target)
  } catch (error) {
    if (error !== null && typeof error === 'object' && 'code' in error && error.code === 'EEXIST') {
      throw new IdeOperationError('already-exists', `The directory already exists: ${target}`)
    }
    throw failure(error, target)
  }
  return listDirectories(target, true, limit)
}
