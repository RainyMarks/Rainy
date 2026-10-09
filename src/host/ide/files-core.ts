/** Shared validation and failures for the authenticated human IDE. */
import { isAbsolute, relative, sep } from 'node:path'
import type { IdeFilesErrorCode, IdeFilesFailure, IdeFileVersion, IdeWorkspaceState } from '../../shared/ide-files-protocol.ts'

/** Typed IDE failure; HTTP integration projects only the documented failure fields. */
export class IdeOperationError extends Error {
  /** @param code - stable failure code. @param message - diagnostic context. @param details - authoritative conflict observations. */
  constructor(readonly code: IdeFilesErrorCode, message: string, readonly details: {
    readonly currentVersion?: IdeFileVersion | null
    readonly currentState?: IdeWorkspaceState
  } = {}) {
    super(message)
    this.name = 'IdeOperationError'
  }

  /** Current on-disk observation when a save was stale. */
  get currentVersion(): IdeFileVersion | null | undefined { return this.details.currentVersion }
  /** Current durable state when a recovery save was stale. */
  get currentState(): IdeWorkspaceState | undefined { return this.details.currentState }
}

/**
 * Validate the portable relative spelling used by editor paths.
 * @param path - slash-separated project-relative path; empty means the root only when allowed.
 * @param allowRoot - whether the workspace root is a valid target.
 * @returns the unchanged validated spelling.
 */
export function ideRelativePath(path: string, allowRoot = false): string {
  if (path === '' && allowRoot) return path
  if (path === '' || /[\u0000-\u001f\\:]/u.test(path) || path.startsWith('/')
    || path.split('/').some(part => part === '' || part === '.' || part === '..')) {
    throw new IdeOperationError('invalid-path', 'IDE paths must be slash-separated paths inside the selected workspace.')
  }
  return path
}

/**
 * Check an absolute resolved target against an absolute canonical root.
 * @param root - canonical workspace root.
 * @param target - freshly resolved target or parent.
 * @returns whether the target remains inside the root.
 */
export function ideContains(root: string, target: string): boolean {
  const remainder = relative(root, target)
  return remainder === '' || (!isAbsolute(remainder) && remainder !== '..' && !remainder.startsWith(`..${sep}`))
}

/**
 * Translate filesystem and cancellation failures for the route's JSON envelope.
 * @param error - operation rejection.
 * @returns a stable error with no arbitrary exception fields.
 */
export function ideFailure(error: unknown): IdeFilesFailure {
  if (error instanceof IdeOperationError) return {
    code: error.code, message: error.message,
    ...error.currentVersion === undefined ? {} : { currentVersion: error.currentVersion },
    ...error.currentState === undefined ? {} : { currentState: error.currentState },
  }
  const code = error !== null && typeof error === 'object' && 'code' in error ? error.code : undefined
  const mapped: IdeFilesErrorCode = code === 'ENOENT' || code === 'FS_NOT_FOUND' ? 'not-found'
    : code === 'EACCES' || code === 'EPERM' || code === 'FS_PERMISSION_DENIED' || code === 'FS_SANDBOX_DENIED' ? 'permission-denied'
      : code === 'EEXIST' || code === 'FS_NOT_OBSERVED' ? 'already-exists'
        : code === 'FS_STALE_VERSION' ? 'version-conflict'
          : code === 'ENOTDIR' || code === 'FS_NOT_DIRECTORY' ? 'not-directory'
            : code === 'FS_NOT_REGULAR_FILE' ? 'not-file'
              : code === 'FS_TOO_LARGE' ? 'too-large'
                : code === 'FS_ABORTED' || error instanceof Error && error.name === 'AbortError' ? 'aborted' : 'io-error'
  return { code: mapped, message: error instanceof Error ? error.message : 'The IDE operation could not be completed.' }
}
