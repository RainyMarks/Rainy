/** Remove program files that tool-pack backups duplicate from saved package inventories; user files stay. */
import { join, relative, sep } from 'node:path'
import { z } from 'zod'
import { checkToolPackCancellation, readToolPackRecord, toolPackHash, toolPackStat, toolPackTree } from './toolpack-files.ts'
import { toolPackMetadataSchema } from './toolpack-format.ts'
import { toolPackFileSystem } from './toolpack-fs.ts'

const { chmod, readdir, rmdir, unlink } = toolPackFileSystem.promises

/** Space reclaimed from backups and the user files left in them. */
export interface ToolPackPruneResult {
  readonly removedFiles: number
  readonly removedBytes: number
  readonly keptFiles: number
}

async function removeFile(path: string): Promise<void> {
  try { await unlink(path) } catch (error) {
    // Windows refuses to delete read-only files that were moved out of a tool directory.
    if (!(error instanceof Error && 'code' in error && error.code === 'EPERM')) throw error
    await chmod(path, 0o666)
    await unlink(path)
  }
}

async function removeEmptyDirectories(path: string): Promise<boolean> {
  const entry = await toolPackStat(path)
  if (!entry?.isDirectory()) return false
  let empty = true
  for (const name of await readdir(path)) if (!await removeEmptyDirectories(join(path, name))) empty = false
  if (empty) await rmdir(path)
  return empty
}

/**
 * Delete backup files whose path, size and SHA-256 match a saved package inventory, then remove emptied directories.
 * The caller holds the installation lock; an unfinished switch or rollback keeps every backup.
 * @param installRoot Directory that owns `.rainy-toolpack`.
 * @param options Backups to prune (all when omitted) and cancellation.
 * @returns Reclaimed bytes and the number of user files kept.
 */
export async function pruneToolPackBackups(
  installRoot: string, options: { readonly transactions?: readonly string[]; readonly signal?: AbortSignal } = {},
): Promise<ToolPackPruneResult> {
  const stateRoot = join(installRoot, '.rainy-toolpack')
  const backups = join(stateRoot, 'backups')
  const result = { removedFiles: 0, removedBytes: 0, keptFiles: 0 }
  if (!(await toolPackStat(backups))?.isDirectory()) return result
  const journal = z.object({ phase: z.string() }).safeParse(await readToolPackRecord(join(stateRoot, 'journal.json')) ?? { phase: 'committed' })
  if (!journal.success || !['staging', 'prepared', 'committed', 'rolled-back'].includes(journal.data.phase)) return result
  const known = new Map<string, { bytes: number; sha256: string }[]>()
  const manifests = join(stateRoot, 'manifests')
  for (const name of (await toolPackStat(manifests))?.isDirectory() ? await readdir(manifests) : []) {
    const parsed = /^[a-f0-9]{64}\.json$/.test(name)
      ? toolPackMetadataSchema.safeParse(await readToolPackRecord(join(manifests, name))) : undefined
    if (!parsed?.success || `${parsed.data.id}.json` !== name) continue
    for (const file of parsed.data.files) {
      const key = file.path.toLowerCase()
      known.set(key, [...known.get(key) ?? [], { bytes: file.bytes, sha256: file.sha256 }])
    }
  }
  for (const transaction of await readdir(backups)) {
    if (!/^[a-f0-9-]{36}$/.test(transaction) || options.transactions && !options.transactions.includes(transaction)) continue
    const root = join(backups, transaction)
    for (const file of await toolPackTree(root, options.signal)) {
      checkToolPackCancellation(options.signal)
      const candidates = known.get(relative(root, file.path).split(sep).join('/').toLowerCase())?.filter(entry => entry.bytes === file.bytes)
      if (candidates?.length) {
        const sha256 = await toolPackHash(file.path, options.signal)
        if (candidates.some(entry => entry.sha256 === sha256)) {
          await removeFile(file.path)
          result.removedFiles++
          result.removedBytes += file.bytes
          continue
        }
      }
      result.keptFiles++
    }
    await removeEmptyDirectories(root)
  }
  return result
}
