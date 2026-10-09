/** Bounded filename discovery for Quick Open without reading file contents or following directory links. */
import { lstat, opendir, realpath } from 'node:fs/promises'
import { relative, resolve } from 'node:path'
import type { IdeFileSearch } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'
import { ideContains } from './ide-files-core.ts'
import type { IdeFilesConfig } from './ide-files.ts'

function transient(error: unknown): boolean {
  return error !== null && typeof error === 'object' && 'code' in error
    && (error.code === 'ENOENT' || error.code === 'ENOTDIR' || error.code === 'EACCES' || error.code === 'EPERM')
}

/**
 * Find regular files by case-insensitive path tokens, within the registered directory.
 * @param root - freshly resolved workspace root.
 * @param query - space-separated filename or path fragments; an empty query lists initial files.
 * @param requestedLimit - optional result cap, clamped to the deployment budget.
 * @param config - explicit scan, result, deadline and excluded-directory budgets.
 * @param signal - caller cancellation, checked between filesystem operations.
 * @param now - clock for the cooperative deadline.
 * @returns project-relative paths and whether any part of the tree was omitted.
 */
export async function searchIdeFiles(
  root: string, query: string, requestedLimit: number | undefined,
  config: Pick<IdeFilesConfig, 'searchResultLimit' | 'searchMaxEntries' | 'searchTimeoutMs' | 'searchExcludedDirectories'>,
  signal?: AbortSignal, now: () => number = Date.now,
): Promise<IdeFileSearch> {
  const tokens = query.trim().toLowerCase().replaceAll('\\', '/').split(/\s+/u).filter(Boolean)
  const limit = Math.min(requestedLimit ?? config.searchResultLimit, config.searchResultLimit)
  const excluded = new Set(config.searchExcludedDirectories)
  const deadline = now() + config.searchTimeoutMs
  const pending = ['']
  const paths: string[] = []
  let scanned = 0
  let truncated = false
  const expired = () => {
    signal?.throwIfAborted()
    return now() >= deadline
  }
  while (pending.length > 0) {
    if (expired()) return { paths: paths.sort(), truncated: true }
    const path = pending.pop()
    if (path === undefined) break
    const absolute = resolve(root, path)
    try {
      const info = await lstat(absolute)
      if (expired()) return { paths: paths.sort(), truncated: true }
      if (!info.isDirectory() || info.isSymbolicLink()) continue
      const canonical = await realpath(absolute)
      if (expired()) return { paths: paths.sort(), truncated: true }
      if (!ideContains(root, canonical) || relative(absolute, canonical) !== '') continue
      const directory = await opendir(canonical)
      try {
        for (;;) {
          if (expired()) return { paths: paths.sort(), truncated: true }
          const entry = await directory.read()
          if (expired()) return { paths: paths.sort(), truncated: true }
          if (entry === null) break
          if (scanned === config.searchMaxEntries) return { paths: paths.sort(), truncated: true }
          scanned++
          const child = path === '' ? entry.name : `${path}/${entry.name}`
          if (entry.isDirectory() && !entry.isSymbolicLink() && !excluded.has(entry.name)) pending.push(child)
          if (!entry.isFile() || !tokens.every(token => child.toLowerCase().includes(token))) continue
          paths.push(child)
          if (paths.length === limit) return { paths: paths.sort(), truncated: true }
        }
      } finally { await directory.close() }
    } catch (error) {
      signal?.throwIfAborted()
      if (!transient(error)) throw error
      truncated = true
    }
  }
  signal?.throwIfAborted()
  return { paths: paths.sort(), truncated }
}
