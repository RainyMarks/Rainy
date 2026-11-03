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
  /** How to install, run, test or build the project; may hold commands in backticks. Empty before 4.8. */
  readonly run?: readonly string[] | undefined
  /** Files to read first, in reading order. */
  readonly start?: readonly TourPlace[] | undefined
  /** What the project does, step by step. */
  readonly flows?: readonly TourFlow[] | undefined
  /** Likely changes and where to make them. */
  readonly tasks?: readonly TourTask[] | undefined
  /** Code, test and configuration files added and removed since it was written, when any were. */
  readonly drift?: { readonly added: number; readonly removed: number } | undefined
}

/** A file or directory the tour points to, with what to find there. */
export interface TourPlace {
  readonly text: string
  /** Path under the project's primary directory; absent when the model named no file that exists. */
  readonly path?: string | undefined
  /** 1-based line in that file. */
  readonly line?: number | undefined
}

/** Something the project does, step by step. */
export interface TourFlow {
  readonly title: string
  readonly steps: readonly TourPlace[]
}

/** A likely change and where to make it. */
export interface TourTask {
  readonly task: string
  readonly places: readonly TourPlace[]
}

/** One thing the agent did in a turn. */
export interface FootprintStep {
  readonly action: 'read' | 'create' | 'edit' | 'delete' | 'search' | 'command' | 'web' | 'agent' | 'other'
  /** A command's description, a search pattern, a query, a path or the tool's name. */
  readonly label: string
  /** Absolute path on the Host of the file or directory the step names. */
  readonly path?: string | undefined
  /** `true` once it succeeded, `false` when it failed, absent while it runs. */
  readonly ok?: boolean | undefined
}

/** One request of the user and what the agent did for it. */
export interface FootprintTurn {
  readonly id: string
  /** The start of the request. */
  readonly text: string
  readonly ts: number
  readonly steps: readonly FootprintStep[]
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
