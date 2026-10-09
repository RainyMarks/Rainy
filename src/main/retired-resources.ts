/** Resources that earlier installers shipped and this carrier downloads on demand or no longer uses. */
import { rm } from 'node:fs/promises'
import { join } from 'node:path'
import { toolPackStat } from './toolpack-files.ts'

/** Names below the resource directory; installer updates overwrite files but never delete these. */
export const RETIRED_RESOURCES = ['strata-runtime', 'php', 'linux-runtime.tar.gz', 'native-tools-metadata.json',
  'native-tools-download.json', 'native-tools-catalog.json'] as const

/**
 * Delete retired resources left by an earlier version; missing entries are skipped.
 * @param resourceRoot - the packaged carrier's resource directory.
 * @returns names that were removed.
 */
export async function removeRetiredResources(resourceRoot: string): Promise<string[]> {
  const removed: string[] = []
  for (const name of RETIRED_RESOURCES) {
    const path = join(resourceRoot, name)
    const entry = await toolPackStat(path)
    if (!entry || entry.isSymbolicLink()) continue
    await rm(path, { recursive: entry.isDirectory(), force: true, maxRetries: 3 })
    removed.push(name)
  }
  return removed
}
