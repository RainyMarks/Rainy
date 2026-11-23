/** The `ide` method's project and file operations the viewer pages make (`native/crates/protocol/src/ide.rs` has all). */
import type { Branded } from './brand.ts'

/** Durable project identity. */
export type WorkspaceId = Branded<'WorkspaceId'>
/** Directory identity within one project. */
export type IdeRootId = Branded<'RainyIdeRootId'>
/** One directory mounted into a project. The primary directory is the chats' working directory. */
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

/** A registered project. */
export interface IdeWorkspace {
  readonly workspaceId: WorkspaceId
  readonly path: string
  readonly title: string
  /** Absent on legacy snapshots, which contain only the primary directory. */
  readonly roots?: readonly IdeWorkspaceRoot[] | undefined
  readonly pinned?: boolean | undefined
}

/** A direct child; paths always use slash-separated project-relative spelling. */
export interface IdeFileEntry {
  readonly name: string
  readonly path: string
  readonly kind: 'file' | 'directory' | 'symlink' | 'other'
  /** Resolved kind for contained symlinks; absent for broken or escaping links. */
  readonly targetKind?: 'file' | 'directory' | 'other' | undefined
  readonly bytes: number
  /** Opaque observation of the file. */
  readonly version: string
  /** A symlink escaping the workspace is shown but cannot be opened. */
  readonly outsideWorkspace: boolean
}

/** One directory listing. */
export interface IdeDirectory {
  readonly path: string
  readonly entries: readonly IdeFileEntry[]
}

/** One-shot tokenized URL returned when registering a converted Office PDF (`officePdf.register`). */
export interface OfficePdfRegistration {
  readonly token: string
  readonly url: string
  readonly expiresInSecs: number
}

/** Incremental tail slice of a workspace file (`ide` op `files.tail`), used for live-following large training logs. */
export interface FileTailResult {
  readonly workspaceId: WorkspaceId
  readonly path: string
  readonly text: string
  readonly offset: number
  readonly nextOffset: number
  readonly totalBytes: number
  readonly truncated: boolean
  readonly reset: boolean
}

/** Response of `POST /rainy/ide/upload`: confirms the uploaded workspace file's path, size and version. */
export interface FileUploadResult {
  readonly workspaceId: WorkspaceId
  readonly path: string
  readonly bytes: number
  readonly version: string
}

/** File operations pages send through the `ide` RPC method. */
export type IdeFilesRequest =
  | { readonly op: 'workspaces.list' }
  | { readonly op: 'files.list'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  | { readonly op: 'files.mkdir'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  /** A new file with binary content, such as an image pasted into a Markdown document; `base64` holds its bytes. */
  | { readonly op: 'files.createBinary'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string; readonly base64: string }
  /** Incremental tail slice of a workspace file, for live-following large training logs. */
  | { readonly op: 'files.tail'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string; readonly offset?: number | undefined; readonly maxBytes?: number | undefined }
  | { readonly op: 'officePdf.register'; readonly path: string; readonly deleteAfterRead?: boolean | undefined }

/** Successful results keyed by request operation. */
export interface IdeFilesResults {
  'workspaces.list': readonly IdeWorkspace[]
  'files.list': IdeDirectory
  'files.mkdir': IdeFileEntry
  'files.createBinary': IdeFileEntry
  'files.tail': FileTailResult
  'officePdf.register': OfficePdfRegistration
}
