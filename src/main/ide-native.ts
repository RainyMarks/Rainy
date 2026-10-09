/** Windows folder selection mapped into the desktop's selected WSL distribution. */
import { win32 } from 'node:path'

/** The only directory information exposed by the isolated native bridge. */
export interface IdeNativeDirectory {
  readonly path: string
  readonly displayPath: string
}

/**
 * Keep one pending native directory dialog for its owning window.
 * @param choose - open and map that window's directory choice.
 * @returns a chooser sharing its pending completion; success, cancellation and rejection allow another choice.
 */
export function createIdeDirectoryPicker(choose: () => Promise<IdeNativeDirectory | null>): () => Promise<IdeNativeDirectory | null> {
  let pending: Promise<IdeNativeDirectory | null> | undefined
  return () => {
    if (pending !== undefined) return pending
    pending = Promise.resolve().then(choose).finally(() => { pending = undefined })
    return pending
  }
}

/** Map an explicitly selected folder; cancellation creates no workspace.
 * @param choose - native folder picker owned by the current window.
 * @param map - argument-based wslpath invocation in the selected distribution.
 * @returns the Linux path and Windows display spelling, or null after cancellation.
 */
export async function chooseIdeDirectory(
  choose: () => Promise<{ canceled: boolean; filePaths: string[] }>,
  map: (path: string) => Promise<string>,
): Promise<IdeNativeDirectory | null> {
  const result = await choose()
  if (result.canceled || result.filePaths.length === 0) return null
  const displayPath = result.filePaths[0]
  if (!win32.isAbsolute(displayPath) || displayPath.includes('\0')) throw new Error('Invalid selected Windows directory')
  const path = (await map(displayPath)).trim()
  if (!path.startsWith('/') || path.includes('\0') || path.includes('\n')) throw new Error('The selected directory could not be mapped into WSL')
  return { path, displayPath }
}
