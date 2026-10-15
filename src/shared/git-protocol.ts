/** Git state of a project's roots for the Git panel. */
import type { IdeRootId } from './ide-files-protocol.ts'

/** How a file differs, in the index (staged) or in the working tree. */
export type GitChangeKind = 'modified' | 'added' | 'deleted' | 'renamed' | 'copied' | 'untracked' | 'conflicted'

/** One changed file, by its path in the root. */
export interface GitFileChange {
  /** Root-relative path with `/` separators. */
  readonly path: string
  /** The path a rename or copy came from. */
  readonly from?: string | undefined
  /** Change recorded in the index, ready to commit. */
  readonly staged?: GitChangeKind | undefined
  /** Change in the working tree not yet staged. */
  readonly unstaged?: GitChangeKind | undefined
}

/** A root inside a Git repository. */
export interface GitRepository {
  readonly rootId: IdeRootId
  readonly title: string
  /** Current branch, or `null` on a detached HEAD. */
  readonly branch: string | null
  /** Abbreviated commit of HEAD, or `null` before the first commit. */
  readonly head: string | null
  readonly upstream?: string | undefined
  readonly ahead: number
  readonly behind: number
  /** Changes inside the root; the status reads at most {@link GIT_MAX_FILES}. */
  readonly files: readonly GitFileChange[]
  /** More files changed than are listed. */
  readonly truncated: boolean
}

/** Git state of every root of a project. */
export interface GitStatus {
  /** Git could be run at all. */
  readonly available: boolean
  readonly repositories: readonly GitRepository[]
  /** Roots outside any repository. */
  readonly plain: readonly { readonly rootId: IdeRootId; readonly title: string }[]
}

/** Changed files listed per repository at most. */
export const GIT_MAX_FILES = 5000
