/** Resumable, digest-checked downloads of published release pieces joined into one local file. */
import { createReadStream } from 'node:fs'
import { open, rm } from 'node:fs/promises'
import { join } from 'node:path'
import { assertToolPackPath, checkToolPackCancellation, renameToolPackPath, toolPackHash, toolPackStat } from './toolpack-files.ts'

/** A published file split into flat release assets below one pinned release directory. */
export interface ReleaseFile {
  readonly file: string
  readonly bytes: number
  readonly sha256: string
  readonly baseUrl: string
  readonly pieces: readonly { readonly file: string; readonly bytes: number; readonly sha256: string }[]
}

/** Transport and destination chosen by the main process. */
export interface ReleaseDownloadOptions {
  readonly directory: string
  readonly fetch: typeof globalThis.fetch
  readonly signal: AbortSignal
  /** Bytes of this file received so far, including resumed and already complete pieces. */
  readonly progress: (bytes: number) => void
}

/** Whether a local file has the expected size and digest.
 * @param path Absolute file path.
 * @param expected Size and SHA-256.
 * @param signal Optional cancellation.
 * @returns Match.
 */
export async function releaseFileMatches(
  path: string, expected: { bytes: number; sha256: string }, signal?: AbortSignal,
): Promise<boolean> {
  await assertToolPackPath(path)
  const info = await toolPackStat(path)
  return info?.isFile() === true && info.size === expected.bytes && await toolPackHash(path, signal) === expected.sha256
}

async function fetchPiece(target: string, url: string, expected: { bytes: number; sha256: string }, options: ReleaseDownloadOptions,
  progress: (bytes: number) => void): Promise<void> {
  if (await releaseFileMatches(target, expected, options.signal)) { progress(expected.bytes); return }
  const partial = target + '.partial'
  await assertToolPackPath(partial)
  const info = await toolPackStat(partial)
  if (info && (!info.isFile() || info.size > expected.bytes)) throw new Error('下载缓存无效，请清理下载缓存后重试')
  let offset = info?.size ?? 0
  if (offset === expected.bytes) {
    if (await releaseFileMatches(partial, expected, options.signal)) {
      await renameToolPackPath(partial, target)
      progress(expected.bytes)
      return
    }
    await rm(partial)
    offset = 0
  }
  const response = await options.fetch(url, { signal: options.signal, headers: offset ? { Range: `bytes=${offset}-` } : {}, credentials: 'omit' })
  if (!response.ok || !response.body) {
    await response.body?.cancel()
    throw new Error(`下载失败（HTTP ${response.status}），请检查网络后重试`)
  }
  if (response.status === 206) {
    if (response.headers.get('content-range') !== `bytes ${offset}-${expected.bytes - 1}/${expected.bytes}`) {
      await response.body.cancel()
      throw new Error('下载续传响应无效')
    }
  } else if (response.status === 200) offset = 0
  else { await response.body.cancel(); throw new Error('下载响应无效') }
  const output = await open(partial, offset ? 'a' : 'w')
  let received = offset
  progress(received)
  try {
    for await (const value of response.body) {
      checkToolPackCancellation(options.signal)
      received += value.byteLength
      if (received > expected.bytes) throw new Error('下载内容超出清单大小')
      await output.writeFile(value)
      progress(received)
    }
  } finally { await output.close() }
  if (!await releaseFileMatches(partial, expected, options.signal)) {
    await rm(partial)
    throw new Error('下载校验失败，请重试')
  }
  await renameToolPackPath(partial, target)
}

/**
 * Download every piece, then join and verify the file before the pieces are removed; complete files are reused.
 * @param release Published file and its pieces.
 * @param options Destination directory, transport, cancellation and progress.
 * @returns Absolute path of the verified file.
 */
export async function downloadReleaseFile(release: ReleaseFile, options: ReleaseDownloadOptions): Promise<string> {
  const target = join(options.directory, release.file)
  if (await releaseFileMatches(target, release, options.signal)) { options.progress(release.bytes); return target }
  const single = release.pieces.length === 1 && release.pieces[0]?.sha256 === release.sha256 ? release.pieces[0] : undefined
  if (single) {
    await fetchPiece(target, release.baseUrl + single.file, release, options, options.progress)
    return target
  }
  const paths: string[] = []
  let pieceOffset = 0
  for (const piece of release.pieces) {
    const baseline = pieceOffset
    const path = join(options.directory, piece.file)
    await fetchPiece(path, release.baseUrl + piece.file, piece, options, (value) => { options.progress(baseline + value) })
    paths.push(path)
    pieceOffset += piece.bytes
  }
  const partial = target + '.partial'
  await assertToolPackPath(partial)
  const output = await open(partial, 'w')
  try {
    for (const path of paths) for await (const chunk of createReadStream(path)) {
      checkToolPackCancellation(options.signal)
      if (!Buffer.isBuffer(chunk)) throw new Error('下载分片读取格式无效')
      await output.writeFile(chunk)
    }
  } finally { await output.close() }
  if (!await releaseFileMatches(partial, release, options.signal)) {
    await rm(partial)
    throw new Error('下载文件校验失败，请重试')
  }
  await renameToolPackPath(partial, target)
  for (const path of paths) await rm(path)
  return target
}
