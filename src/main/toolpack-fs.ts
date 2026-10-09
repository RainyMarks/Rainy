/** Raw disk access for opaque native-tool payloads, including third-party ASAR archives. */
import * as nodeFileSystem from 'node:fs'
import { createRequire } from 'node:module'

/**
 * Electron's unmodified filesystem for payload checks, staging and recovery; plain Node uses its standard filesystem.
 * Carrier page and module loading keep Electron's normal ASAR support.
 */
export const toolPackFileSystem: typeof nodeFileSystem = process.versions.electron
  ? createRequire(process.execPath)('original-fs') as typeof nodeFileSystem
  : nodeFileSystem
