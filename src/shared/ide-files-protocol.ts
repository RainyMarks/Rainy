/** Browser-safe requests and results for the human IDE; identities never depend on a chat. */
import type { Branded } from './brand.ts'
import type { IdeExecutionConfiguration } from './ide-execution-protocol.ts'

/** Durable project identity. */
export type WorkspaceId = Branded<'WorkspaceId'>
/** Chat identity. */
export type SessionId = string
/** Directory identity within one project; it does not identify a separate chat group. */
export type IdeRootId = Branded<'RainyIdeRootId'>
/** One directory mounted into a project. The primary directory remains the Session cwd. */
export interface IdeWorkspaceRoot {
  readonly rootId: IdeRootId
  readonly path: string
  readonly title: string
  readonly primary: boolean
}
/** A file or directory relative to a mounted project root. */
export interface IdeFileReference {
  readonly rootId?: IdeRootId | undefined
  readonly path: string
}
/** Opaque observation returned by file reads and destructive-operation previews. */
export type IdeFileVersion = Branded<'RainyIdeFileVersion'>
/** One-use confirmation issued for an exact observed deletion. */
export type IdeDeleteToken = Branded<'RainyIdeDeleteToken'>

/** Registered directory, usable without creating or opening a chat. */
export interface IdeWorkspace {
  readonly workspaceId: WorkspaceId
  readonly path: string
  readonly title: string
  /** Absent on legacy snapshots, which contain only the primary directory. */
  readonly roots?: readonly IdeWorkspaceRoot[] | undefined
}

/** A direct child; paths always use slash-separated project-relative spelling. */
export interface IdeFileEntry {
  readonly name: string
  readonly path: string
  readonly kind: 'file' | 'directory' | 'symlink' | 'other'
  /** Resolved kind for contained symlinks; absent for broken or escaping links. */
  readonly targetKind?: 'file' | 'directory' | 'other' | undefined
  readonly bytes: number
  readonly version: IdeFileVersion
  /** A symlink escaping the workspace is shown but cannot be opened. */
  readonly outsideWorkspace: boolean
}

/** Complete editable UTF-8 text, or metadata explaining why editing is unavailable. */
export interface IdeFileDocument {
  readonly workspaceId: WorkspaceId
  readonly path: string
  readonly version: IdeFileVersion
  readonly bytes: number
  /** Null for a binary, unsupported encoding, or over-budget file; never truncated source. */
  readonly content: string | null
  readonly bom: boolean
  readonly eol: 'lf' | 'crlf' | 'mixed'
  readonly readOnlyReason: 'binary' | 'too-large' | 'unsupported-encoding' | null
  /** Bounded display bytes for read-only files; this prefix is never accepted as editable source. */
  readonly preview?: {
    readonly kind: 'utf8' | 'hex'
    readonly text: string
    readonly bytesRead: number
    readonly truncated: boolean
  } | undefined
}

/** Matching project-relative files; truncation includes scan, result and time budgets. */
export interface IdeFileSearch {
  readonly paths: readonly string[]
  readonly truncated: boolean
}

/** One directory listing. */
export interface IdeDirectory {
  readonly path: string
  readonly entries: readonly IdeFileEntry[]
}

/** Preview of the exact path and subtree the user is asked to remove. */
export interface IdeDeletePreview {
  readonly path: string
  readonly kind: IdeFileEntry['kind']
  readonly entries: number
  readonly bytes: number
  readonly token: IdeDeleteToken
}

/** Whole-file comparison for Monaco; unavailable content stays read-only. */
export interface IdeFileDiff {
  readonly path: string
  readonly base: string | null
  readonly current: string | null
  readonly version: IdeFileVersion | null
  readonly status: 'modified' | 'added' | 'deleted' | 'unchanged' | 'unavailable'
  readonly reason?: 'not-git' | 'binary' | 'too-large' | 'unsupported-encoding' | undefined
}

/** Observations of opened paths, including deletion, without a model turn. */
export interface IdeFileChange {
  readonly path: string
  readonly version: IdeFileVersion | null
  readonly kind: IdeFileEntry['kind'] | 'missing'
}

/** Persisted cursor and scroll values for one editor tab. */
export interface IdeEditorTab {
  readonly rootId?: IdeRootId | undefined
  readonly path: string
  readonly kind: 'file' | 'diff'
  readonly cursor?: { readonly line: number; readonly column: number } | undefined
  readonly scroll?: { readonly top: number; readonly left: number } | undefined
}

/** Recovery copy kept independently of the original file; null base means an unsaved new file. */
export interface IdeDirtyBuffer {
  readonly rootId?: IdeRootId | undefined
  readonly path: string
  readonly content: string
  readonly baseVersion: IdeFileVersion | null
  readonly bom: boolean
  readonly eol: 'lf' | 'crlf' | 'mixed'
}

/** Layout values are per workspace and do not change any saved chat. */
export interface IdeLayoutState {
  readonly sidebarWidth: number
  readonly agentWidth: number
  readonly bottomHeight: number
  readonly sidebarVisible: boolean
  readonly agentVisible: boolean
  readonly bottomVisible: boolean
  readonly bottomTab: 'terminal' | 'problems' | 'output' | 'debug'
}

/** Versioned workspace state. Only documented editor fields are persisted. */
export interface IdeWorkspaceStateData {
  /** Remembered chat selection only; reading state never starts or resumes it. */
  readonly lastSessionId: SessionId | null
  readonly tabs: readonly IdeEditorTab[]
  readonly activePath: string | null
  readonly activeRootId?: IdeRootId | undefined
  readonly expandedPaths: readonly string[]
  /** Expanded directories in attached roots; primary expansions retain their original spelling. */
  readonly expandedRoots?: readonly IdeFileReference[] | undefined
  readonly buffers: readonly IdeDirtyBuffer[]
  readonly layout: IdeLayoutState
  readonly execution?: IdeExecutionConfiguration | undefined
}

/** Atomic snapshot; revision zero represents a workspace with no saved IDE state. */
export interface IdeWorkspaceState {
  readonly version: 1
  readonly revision: number
  readonly data: IdeWorkspaceStateData
}

/** Last successfully selected project, retained independently of the renderer origin and chat state. */
export interface IdeWorkspaceSelection {
  readonly version: 1
  readonly workspaceId: WorkspaceId | null
}

/** One level of the in-app directory browser, outside any project. */
export interface IdeDirectoryListing {
  /** Absolute directory shown. */
  readonly path: string
  /** Parent directory, or `null` at a filesystem root. */
  readonly parent: string | null
  /** Filesystem roots to offer as shortcuts (drive letters on Windows, `/` and the home directory on Linux). */
  readonly roots: readonly string[]
  readonly entries: readonly { readonly name: string; readonly path: string; readonly hidden: boolean }[]
}

/** File operations served through the `ide` RPC method. */
export type IdeFilesRequest =
  | { readonly op: 'directories.list'; readonly path?: string | undefined; readonly showHidden?: boolean | undefined }
  | { readonly op: 'directories.create'; readonly path: string }
  | { readonly op: 'workspaces.list' }
  | { readonly op: 'workspaces.open'; readonly path: string }
  | { readonly op: 'workspaces.attach'; readonly workspaceId: WorkspaceId; readonly path: string }
  | { readonly op: 'workspaces.removeRoot'; readonly workspaceId: WorkspaceId; readonly rootId: IdeRootId }
  | { readonly op: 'workspaces.rename'; readonly workspaceId: WorkspaceId; readonly title: string }
  | { readonly op: 'workspaces.remove'; readonly workspaceId: WorkspaceId }
  | { readonly op: 'files.list'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  | { readonly op: 'files.read'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  | { readonly op: 'files.search'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly query: string; readonly limit?: number | undefined }
  | { readonly op: 'files.save'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string; readonly content: string; readonly expectedVersion: IdeFileVersion }
  | { readonly op: 'files.create'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string; readonly content: string }
  | { readonly op: 'files.mkdir'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  | { readonly op: 'files.rename'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string; readonly destination: string; readonly expectedVersion: IdeFileVersion }
  | { readonly op: 'files.deletePreview'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  | { readonly op: 'files.delete'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string; readonly token: IdeDeleteToken }
  | { readonly op: 'files.diff'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  | { readonly op: 'files.changes'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly paths: readonly string[] }

/** Workspace state operations share the IDE route but have their own durable revision. */
export type IdeStateRequest =
  | { readonly op: 'state.read'; readonly workspaceId: WorkspaceId }
  | { readonly op: 'state.save'; readonly workspaceId: WorkspaceId; readonly baseRevision: number; readonly data: IdeWorkspaceStateData }
  | { readonly op: 'state.selection.read' }
  | { readonly op: 'state.selection.save'; readonly workspaceId: WorkspaceId | null }

/** Successful results keyed by request operation for typed clients. */
export interface IdeFilesResults {
  'directories.list': IdeDirectoryListing
  'directories.create': IdeDirectoryListing
  'workspaces.list': readonly IdeWorkspace[]
  'workspaces.open': IdeWorkspace
  'workspaces.attach': IdeWorkspace
  'workspaces.removeRoot': IdeWorkspace
  'workspaces.rename': IdeWorkspace
  'workspaces.remove': { readonly workspaceId: WorkspaceId; readonly removed: true }
  'files.list': IdeDirectory
  'files.read': IdeFileDocument
  'files.search': IdeFileSearch
  'files.save': IdeFileDocument
  'files.create': IdeFileDocument
  'files.mkdir': IdeFileEntry
  'files.rename': IdeFileEntry
  'files.deletePreview': IdeDeletePreview
  'files.delete': { readonly path: string; readonly deleted: true }
  'files.diff': IdeFileDiff
  'files.changes': readonly IdeFileChange[]
  'state.read': IdeWorkspaceState
  'state.save': IdeWorkspaceState
  'state.selection.read': IdeWorkspaceSelection
  'state.selection.save': IdeWorkspaceSelection
}

/** Stable failure codes; callers localize them instead of parsing filesystem messages. */
export type IdeFilesErrorCode = 'invalid-request' | 'workspace-not-found' | 'workspace-unavailable'
  | 'root-overlap'
  | 'invalid-path' | 'outside-workspace' | 'not-found' | 'not-file' | 'not-directory' | 'already-exists'
  | 'read-only' | 'too-large' | 'version-conflict' | 'confirmation-required' | 'revision-conflict'
  | 'state-too-large' | 'permission-denied' | 'aborted' | 'closed' | 'io-error'

/** Failure response details retain the authoritative conflict observation. */
export interface IdeFilesFailure {
  readonly code: IdeFilesErrorCode
  readonly message: string
  readonly currentVersion?: IdeFileVersion | null | undefined
  readonly currentState?: IdeWorkspaceState | undefined
}
