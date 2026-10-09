/** Version-checked local file access and no-replace rename for the human editor. */
import { randomUUID } from 'node:crypto'
import { createReadStream, type BigIntStats, type Stats } from 'node:fs'
import { chmod, link, lstat, open, realpath, rename, rm, stat, unlink } from 'node:fs/promises'
import { basename, dirname, join } from 'node:path'
import { IdeOperationError } from './files-core.ts'

/** A path entry observed without following a final symbolic link. */
export interface IdePathInfo {
  /** Opaque identity and freshness token (`dev:ino:size:mtimeNs:ctimeNs`). */
  readonly version: string
  readonly type: 'file' | 'directory' | 'symlink' | 'other'
  readonly size: number
}

/** A resolved target observed after following links. */
export interface IdeTargetInfo {
  readonly version: string
  readonly type: 'file' | 'directory' | 'other'
  readonly size: number
}

/** Guarded write: create only when absent, or replace only the observed version. */
export type IdeWriteIntent = { readonly kind: 'createIfAbsent' } | { readonly kind: 'replaceIfVersion'; readonly version: string }

function errnoOf(error: unknown): string | undefined {
  return error !== null && typeof error === 'object' && 'code' in error && typeof error.code === 'string' ? error.code : undefined
}

function absent(error: unknown): boolean {
  const code = errnoOf(error)
  return code === 'ENOENT' || code === 'ENOTDIR'
}

function versionOf(info: BigIntStats): string {
  return `${info.dev}:${info.ino}:${info.size}:${info.mtimeNs}:${info.ctimeNs}`
}

function typeOf(info: Stats | BigIntStats): 'file' | 'directory' | 'other' {
  return info.isFile() ? 'file' : info.isDirectory() ? 'directory' : 'other'
}

function aborted(signal: AbortSignal | undefined): void {
  if (signal?.aborted) throw new IdeOperationError('aborted', 'The IDE operation was cancelled.')
}

async function collect(stream: AsyncIterable<unknown>, limit: number, tooLarge: () => Error): Promise<Buffer> {
  const chunks: Buffer[] = []
  let bytes = 0
  try {
    for await (const chunk of stream) {
      const buffer = chunk as Buffer
      bytes += buffer.length
      if (bytes > limit) throw tooLarge()
      chunks.push(buffer)
    }
  } catch (error) {
    if (error instanceof Error && error.name === 'AbortError') throw new IdeOperationError('aborted', 'The IDE operation was cancelled.')
    throw error
  }
  return Buffer.concat(chunks, bytes)
}

/** Node filesystem access used by the IDE; writes to one target are serialized and published atomically. */
export class LocalIdeFileSystem {
  /** Observes the synced staging file before it is published; a rejection abandons the write and removes the staging file. */
  onStaged: ((temporary: string) => Promise<void> | void) | undefined
  private readonly locks = new Map<string, Promise<unknown>>()

  /**
   * Inspect a path entry without following a final link.
   * @param path Absolute path.
   * @returns The entry, or `undefined` when it or a parent is missing.
   */
  async lstat(path: string): Promise<IdePathInfo | undefined> {
    let info: BigIntStats
    try { info = await lstat(path, { bigint: true }) } catch (error) {
      if (absent(error)) return undefined
      throw error
    }
    return { version: versionOf(info), type: info.isSymbolicLink() ? 'symlink' : typeOf(info), size: Number(info.size) }
  }

  /**
   * Inspect a target after following links.
   * @param path Absolute path, usually a canonical target from {@link resolve}.
   * @returns The target, or `undefined` when it is missing.
   */
  async stat(path: string): Promise<IdeTargetInfo | undefined> {
    let info: BigIntStats
    try { info = await stat(path, { bigint: true }) } catch (error) {
      if (absent(error)) return undefined
      throw error
    }
    return { version: versionOf(info), type: typeOf(info), size: Number(info.size) }
  }

  /**
   * Canonicalize a path; a missing path keeps its missing suffix below the nearest existing ancestor.
   * @param path Absolute path.
   * @returns The canonical target path.
   */
  async resolve(path: string): Promise<string> {
    const missing: string[] = []
    let current = path
    for (;;) {
      try {
        return join(await realpath(current), ...missing)
      } catch (error) {
        if (errnoOf(error) === 'ENOTDIR') throw new IdeOperationError('not-found', 'A parent path segment is not a directory.')
        if (errnoOf(error) !== 'ENOENT') throw error
        const parent = dirname(current)
        if (parent === current) return path
        missing.unshift(basename(current))
        current = parent
      }
    }
  }

  /**
   * Read a whole regular file.
   * @param path Canonical file path.
   * @param maxBytes Inclusive size limit.
   * @param signal Cancels the read.
   * @returns The raw bytes.
   */
  async readBytes(path: string, maxBytes: number, signal?: AbortSignal): Promise<Uint8Array> {
    aborted(signal)
    const info = await stat(path)
    if (!info.isFile()) throw new IdeOperationError('not-file', 'The selected path is not a regular file.')
    const tooLarge = (): Error => new IdeOperationError('too-large', 'The file exceeds the configured size limit.')
    if (info.size > maxBytes) throw tooLarge()
    return collect(createReadStream(path, { end: maxBytes, ...signal ? { signal } : {} }), maxBytes, tooLarge)
  }

  /**
   * Read at most `range.length` bytes starting at `range.offset`.
   * @param path Canonical file path.
   * @param range Byte window.
   * @param signal Cancels the read.
   * @returns The window's bytes.
   */
  async readByteRange(path: string, range: { readonly offset: number; readonly length: number }, signal?: AbortSignal): Promise<Uint8Array> {
    aborted(signal)
    if (!(await stat(path)).isFile()) throw new IdeOperationError('not-file', 'The selected path is not a regular file.')
    if (range.length === 0) return new Uint8Array(0)
    const stream = createReadStream(path, { start: range.offset, end: range.offset + range.length - 1, ...signal ? { signal } : {} })
    return collect(stream, range.length, () => new IdeOperationError('io-error', 'The file returned more bytes than requested.'))
  }

  /**
   * Publish complete UTF-8 text through a synced sibling staging file.
   * A replacement keeps the file's POSIX mode; a new file gets the default mode (0666 minus the umask).
   * @param path Canonical target path whose parent directory exists.
   * @param content Complete file text.
   * @param intent Create-only, or replace-only-if-unchanged.
   * @param signal Checked before publication.
   */
  writeText(path: string, content: string, intent: IdeWriteIntent, signal?: AbortSignal): Promise<void> {
    const prior = this.locks.get(path) ?? Promise.resolve()
    const result = prior.then(() => this.write(path, content, intent, signal), () => this.write(path, content, intent, signal))
    const tail = result.catch(() => undefined)
    this.locks.set(path, tail)
    void tail.then(() => { if (this.locks.get(path) === tail) this.locks.delete(path) })
    return result
  }

  private async write(path: string, content: string, intent: IdeWriteIntent, signal?: AbortSignal): Promise<void> {
    aborted(signal)
    let existing: BigIntStats | undefined
    try { existing = await stat(path, { bigint: true }) } catch (error) {
      if (!absent(error)) throw error
    }
    if (existing !== undefined && !existing.isFile()) throw new IdeOperationError('not-file', 'The destination is not a regular file.')
    if (intent.kind === 'createIfAbsent' && existing !== undefined) throw new IdeOperationError('already-exists', 'The destination already exists.')
    if (intent.kind === 'replaceIfVersion' && (existing === undefined || versionOf(existing) !== intent.version)) {
      throw new IdeOperationError('version-conflict', 'The file changed after it was opened. Reload or compare before saving.')
    }
    const temporary = join(dirname(path), `.${basename(path)}.${randomUUID()}.tmp`)
    const handle = await open(temporary, 'wx', 0o666)
    try {
      try {
        await handle.writeFile(content, 'utf8')
        await handle.sync()
      } finally {
        await handle.close()
      }
      if (existing !== undefined) await chmod(temporary, Number(existing.mode & 0o7777n))
      await this.onStaged?.(temporary)
      aborted(signal)
      if (intent.kind === 'createIfAbsent') {
        // A hard link publishes without replacing a file another writer created after the check above.
        await link(temporary, path)
        await unlink(temporary)
      } else {
        await rename(temporary, path)
      }
    } catch (error) {
      await rm(temporary, { force: true }).catch((_cleanupError: unknown) => { /* The original failure is more useful than a residue error. */ })
      if (intent.kind === 'createIfAbsent' && errnoOf(error) === 'EEXIST') throw new IdeOperationError('already-exists', 'The destination already exists.')
      throw error
    }
  }
}

function exists(path: string): Error {
  return Object.assign(new Error(`Cannot rename to "${path}": the destination already exists.`), { code: 'EEXIST', path })
}

/**
 * Rename within one filesystem without replacing an existing destination.
 * Regular files (and links on Linux) are published with a hard link, so a competing destination fails with EEXIST.
 * Directories, Windows links and filesystems without hard links fall back to an absence check followed by `rename`;
 * a destination created between that check and the rename can still be replaced (an empty directory on Linux).
 * @param source Validated absolute source; a final link itself is moved.
 * @param destination Validated absolute destination whose parent exists.
 * @param kind Entry type of the source path.
 */
export async function renameNoReplace(source: string, destination: string, kind: IdePathInfo['type']): Promise<void> {
  if (kind === 'file' || (kind === 'symlink' && process.platform !== 'win32')) {
    let linked = false
    try {
      await link(source, destination)
      linked = true
    } catch (error) {
      if (errnoOf(error) === 'EEXIST') throw error
    }
    if (linked) {
      try {
        await unlink(source)
      } catch (error) {
        await unlink(destination).catch((_cleanupError: unknown) => { /* The unlink failure below explains the outcome. */ })
        throw error
      }
      return
    }
  }
  let present = true
  try { await lstat(destination) } catch (error) {
    if (!absent(error)) throw error
    present = false
  }
  if (present) throw exists(destination)
  await rename(source, destination)
}
