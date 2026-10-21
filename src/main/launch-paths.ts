/** Files and folders handed to Rainy on its command line: by Explorer's "Open with Rainy", by a file whose default app is
 *  Rainy, or from a terminal. */
import { stat } from 'node:fs/promises'
import { win32 } from 'node:path'

/** One path to open, as the Host sees it. */
export interface LaunchEntry {
  readonly path: string
  readonly kind: 'file' | 'directory'
}

/**
 * @param argv The process's arguments, or a second instance's.
 * @param workingDirectory Where that process was started, for relative paths.
 * @param defaultApp Whether Electron runs an unpackaged app, whose second argument is the app folder.
 * @returns Absolute Windows paths, in the order given; switches (Chromium's, Rainy's and the updater's) are skipped.
 */
export function launchPaths(argv: readonly string[], workingDirectory: string, defaultApp: boolean): string[] {
  const paths: string[] = []
  let positional = false
  for (const argument of argv.slice(defaultApp ? 2 : 1)) {
    if (!positional && argument === '--') { positional = true; continue }
    if (!positional && argument.startsWith('-')) continue
    if (argument.trim() === '' || argument.includes('\0')) continue
    paths.push(win32.resolve(workingDirectory, argument))
  }
  return paths
}

/**
 * Find out which of the paths exist and whether each is a file or a folder.
 * @param paths Absolute Windows paths.
 * @returns The entries that exist, and the paths that do not.
 */
export async function inspectLaunchPaths(paths: readonly string[]): Promise<{ entries: LaunchEntry[]; missing: string[] }> {
  const entries: LaunchEntry[] = []
  const missing: string[] = []
  for (const path of paths) {
    const info = await stat(path).catch(() => undefined)
    if (info === undefined || (!info.isFile() && !info.isDirectory())) missing.push(path)
    else entries.push({ path, kind: info.isDirectory() ? 'directory' : 'file' })
  }
  return { entries, missing }
}
