/** Shared validation and failures for the human IDE. */
import { isAbsolute, relative, sep } from 'node:path'
import type { IdeFilesErrorCode, IdeFilesFailure, IdeFileVersion, IdeWorkspaceState } from '../../shared/ide-files-protocol.ts'

/** Typed IDE failure; the `ide` RPC method reports only the documented failure fields. */
export class IdeOperationError extends Error {
  /** @param code Stable failure code. @param message Diagnostic context. @param details Authoritative conflict observations. */
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
 * @param path Slash-separated project-relative path; empty means the root only when allowed.
 * @param allowRoot Whether the workspace root is a valid target.
 * @returns The unchanged validated spelling.
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
 * @param root Canonical workspace root.
 * @param target Freshly resolved target or parent.
 * @returns Whether the target remains inside the root.
 */
export function ideContains(root: string, target: string): boolean {
  const remainder = relative(root, target)
  return remainder === '' || (!isAbsolute(remainder) && remainder !== '..' && !remainder.startsWith(`..${sep}`))
}

/**
 * Translate filesystem and cancellation failures to stable codes.
 * @param error Operation rejection.
 * @returns A stable error with no arbitrary exception fields.
 */
export function ideFailure(error: unknown): IdeFilesFailure {
  if (error instanceof IdeOperationError) return {
    code: error.code, message: error.message,
    ...error.currentVersion === undefined ? {} : { currentVersion: error.currentVersion },
    ...error.currentState === undefined ? {} : { currentState: error.currentState },
  }
  const code = error !== null && typeof error === 'object' && 'code' in error ? error.code : undefined
  const mapped: IdeFilesErrorCode = code === 'ENOENT' ? 'not-found'
    : code === 'EACCES' || code === 'EPERM' ? 'permission-denied'
      : code === 'EEXIST' || code === 'ENOTEMPTY' ? 'already-exists'
        : code === 'ENOTDIR' ? 'not-directory'
          : code === 'EISDIR' ? 'not-file'
            : code === 'ABORT_ERR' || error instanceof Error && error.name === 'AbortError' ? 'aborted' : 'io-error'
  return { code: mapped, message: error instanceof Error ? error.message : 'The IDE operation could not be completed.' }
}
