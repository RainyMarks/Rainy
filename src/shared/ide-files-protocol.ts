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

/** File operations pages send through the `ide` RPC method. */
export type IdeFilesRequest =
  | { readonly op: 'workspaces.list' }
  | { readonly op: 'files.list'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  | { readonly op: 'files.mkdir'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string }
  /** A new file with binary content, such as an image pasted into a Markdown document; `base64` holds its bytes. */
  | { readonly op: 'files.createBinary'; readonly workspaceId: WorkspaceId; readonly rootId?: IdeRootId | undefined; readonly path: string; readonly base64: string }

/** Successful results keyed by request operation. */
export interface IdeFilesResults {
  'workspaces.list': readonly IdeWorkspace[]
  'files.list': IdeDirectory
  'files.mkdir': IdeFileEntry
  'files.createBinary': IdeFileEntry
}
