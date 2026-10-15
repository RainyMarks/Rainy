/** The project map: every file of a project with its imports, what the project is built with, and an optional AI explanation. */
import type { WorkspaceId } from './ide-files-protocol.ts'

/** What a file is, for colors and for which files make up the architecture. */
export type MapFileKind = 'code' | 'test' | 'doc' | 'config' | 'asset' | 'other'

/** One file of the project. */
export interface MapFile {
  /** Path under the project's primary directory, with `/` separators. */
  readonly path: string
  /** Language id such as `typescript` or `python`; empty when unknown. */
  readonly language: string
  readonly kind: MapFileKind
  /** Lines of text; an estimate for large files and 0 for binary ones. */
  readonly lines: number
  readonly bytes: number
}

/** A language, framework, library or tool the project uses. */
export interface MapStackItem {
  readonly name: string
  readonly kind: 'language' | 'framework' | 'library' | 'tool'
}

/** An external package and the files that import it. */
export interface MapPackage {
  readonly name: string
  /** Indexes into {@link ProjectMap.files}. */
  readonly files: readonly number[]
}

/** What the AI wrote about the project's structure. */
export interface MapExplanation {
  readonly generatedAt: number
  readonly locale: 'zh' | 'en'
  /** A few sentences on what the project is and how it is put together. */
  readonly summary: string
  /** One short line per directory or file, keyed by its path ('' is the whole project). */
  readonly modules: Readonly<Record<string, string>>
  /** Provider and model that wrote it. */
  readonly model: string
}

/** Everything the map page draws. */
export interface ProjectMap {
  readonly workspaceId: WorkspaceId
  readonly title: string
  readonly generatedAt: number
  /** Files beyond the scan limits were left out, or some were not read. */
  readonly truncated: boolean
  readonly files: readonly MapFile[]
  /** Imports between project files as index pairs into `files`: `[from, to, from, to, …]`. */
  readonly imports: readonly number[]
  readonly packages: readonly MapPackage[]
  readonly stack: readonly MapStackItem[]
  readonly explanation?: MapExplanation | undefined
}
