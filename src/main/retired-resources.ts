/** Resources that earlier installers shipped and this carrier downloads on demand or no longer uses. */
import { lstat, readFile, readdir, rm, rmdir } from 'node:fs/promises'
import { join } from 'node:path'
import { toolPackStat } from './toolpack-files.ts'

/** Names below the resource directory; installer updates overwrite files but never delete these. */
export const RETIRED_RESOURCES = ['strata-runtime', 'php', 'linux-runtime.tar.gz', 'native-tools-metadata.json',
  'native-tools-download.json', 'native-tools-catalog.json', 'native-tools-channel.signed.json', 'native-tools-public-keys.json'] as const

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

/**
 * Delete files below `windows-host` that its inventory does not list. An installer update overwrites the files it ships
 * but never removes ones a newer version dropped, such as the PowerShell 7 that 2.0 carried. Links are left alone, and a
 * folder is removed only when this emptied it, or, for the uninstaller, when it is empty (the package ships empty ones).
 * @param resourceRoot - the packaged carrier's resource directory.
 * @param options - `everything` also removes the listed files and the inventory: the uninstaller's file-by-file removal.
 * @returns bytes removed; 0 when the inventory is missing or unreadable.
 */
export async function pruneWindowsHost(resourceRoot: string, options: { everything?: boolean } = {}): Promise<number> {
  const root = join(resourceRoot, 'windows-host')
  const rootEntry = await toolPackStat(root)
  if (!rootEntry?.isDirectory() || rootEntry.isSymbolicLink()) return 0
  let inventory: unknown
  try { inventory = JSON.parse(await readFile(join(root, 'runtime.json'), 'utf8')) } catch (_unreadable) { return 0 }
  if (inventory === null || typeof inventory !== 'object' || !('files' in inventory) || !Array.isArray(inventory.files)) return 0
  // Windows paths compare without case, so a file whose name only changed case is never mistaken for a stray one.
  const key = (path: string): string => process.platform === 'win32' ? path.toLowerCase() : path
  const listed = new Set<string>([key('runtime.json')])
  for (const file of inventory.files as unknown[]) {
    if (file === null || typeof file !== 'object' || !('path' in file) || typeof file.path !== 'string') return 0
    listed.add(key(file.path))
  }
  if (options.everything === true) listed.clear()
  let removed = 0
  /** @returns whether the folder is now empty and may go: emptied here, or empty while removing everything. */
  const prune = async (directory: string, relative: string): Promise<boolean> => {
    let empty = true
    let changed = false
    for (const entry of await readdir(directory, { withFileTypes: true })) {
      const path = relative === '' ? entry.name : `${relative}/${entry.name}`
      const full = join(directory, entry.name)
      if (entry.isSymbolicLink()) empty = false
      else if (entry.isDirectory()) {
        if (await prune(full, path)) { await rmdir(full); changed = true } else empty = false
      } else if (listed.has(key(path))) empty = false
      else {
        removed += (await lstat(full)).size
        await rm(full, { force: true, maxRetries: 3 })
        changed = true
      }
    }
    return empty && (changed || options.everything === true)
  }
  if (await prune(root, '') && options.everything === true) await rmdir(root)
  return removed
}
