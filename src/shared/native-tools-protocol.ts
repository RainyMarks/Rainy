/** Fixed catalog and narrow desktop operations exposed to Rainy's trusted main window. */
import type { Branded } from './brand.ts'

/** Initial tool identities with built-in localized descriptions. */
export const nativeToolIds = ['yakit', 'cyberchef', '7zip', 'exiftool', 'wireshark',
  'binwalk', 'ffmpeg', 'audacity', 'stegsolve', 'pngcheck', 'qrazybox', 'image-lsb-viewer',
  'imagemagick', 'ida', 'x64dbg', 'die', 'imhex', 'jadx', 'dnspy', 'pyinstxtractor-ng',
  'winmerge', 'qalculate', 'sonic-visualiser', 'tesseract-ocr', 'sox', 'tweakpng',
  'multimon-ng', 'gnu-strings', 'gimp', 'curl', 'jq', 'yq', 'sqlite', 'sqlite-browser',
  'qpdf', 'ripgrep', 'bruno', 'pcapfix'] as const

/** Known tool identities and validated IDs supplied by signed publisher catalogs. */
export type NativeToolId = typeof nativeToolIds[number] | Branded<'NativeToolId'>

/** Result of checking the publisher's signed tool channel. */
export interface NativeToolsUpdateState {
  readonly phase: 'unchecked' | 'checking' | 'current' | 'available' | 'error'
  readonly version: string
  readonly error: string
}

/** File availability is separate from completed tool acceptance; 'available' tools are not downloaded yet. */
export interface NativeToolSummary {
  readonly id: NativeToolId
  readonly name: string
  readonly category: 'web' | 'misc' | 'reverse'
  readonly version: string
  readonly launchKind: 'desktop' | 'terminal' | 'web'
  readonly status: 'ready' | 'missing' | 'available'
  readonly verified: boolean
  readonly missing: readonly string[]
  /** Installed tool whose files differ from the newest signed catalog. */
  readonly outdated: boolean
  /** Compressed bytes to download: the whole tool when absent, or its changed parts when outdated. */
  readonly downloadBytes: number
  readonly variants?: readonly { readonly id: 'x32'; readonly name: string; readonly status: NativeToolSummary['status'] }[]
}

/** Per-user preferences are shared by all conversations. */
export interface NativeToolPreferences {
  readonly favorites: readonly NativeToolId[]
  readonly recent: readonly NativeToolId[]
}

/** Current filesystem availability and saved user preferences. */
export interface NativeToolCatalog {
  readonly tools: readonly NativeToolSummary[]
  readonly preferences: NativeToolPreferences
  /** Installed tools describe an older catalog, even when no tool files changed. */
  readonly catalogOutdated: boolean
  /** Compressed bytes that bring every downloaded tool and the catalog to the newest revision. */
  readonly updateBytes: number
}

/** Reports platform acceptance of a launch request; interface readiness and tool acceptance require separate observations. */
export interface NativeToolLaunchResult { readonly ok: boolean; readonly error?: string; readonly warning?: string }

/** Kind of tool operation; 'update' brings installed tools to the newest catalog without adding tools. */
export type NativeToolsOperation = 'install' | 'update' | 'remove' | 'repair'

/** Retained download and installation progress of the latest tool operation. */
export interface NativeToolsDownloadState {
  /** 'verifying' hashes installed files before a repair downloads the damaged parts. */
  readonly phase: 'idle' | 'verifying' | 'downloading' | 'installing' | 'complete' | 'cancelled' | 'error'
  readonly completedBytes: number
  readonly totalBytes: number
  readonly error: string
  readonly operation?: NativeToolsOperation
  /** Tools added or removed by the operation; empty for updates and repairs. */
  readonly tools?: readonly NativeToolId[]
}

/** Preload operations available only to the trusted main window. */
export interface NativeToolsBridge {
  /** Check the signed publisher channel for added or updated tools. @returns update availability. */
  checkToolUpdates(): Promise<NativeToolsUpdateState>
  /** Read local progress without starting a network request. @returns retained download state. */
  getDownloadState(): Promise<NativeToolsDownloadState>
  /**
   * Download and install the selected tools; installed tools are brought to the same catalog, unchanged ones are kept.
   * @param ids - tools to add; empty updates the installed tools only.
   * @returns installation completion.
   */
  installTools(ids: readonly NativeToolId[]): Promise<void>
  /** Remove one installed tool and any runtime no other installed tool uses. @param id - installed tool. @returns completion. */
  removeTool(id: NativeToolId): Promise<void>
  /** Re-download the installed parts whose files are missing or changed. @returns completion. */
  repairTools(): Promise<void>
  /** Cancel the active download or installation safely. @returns settled cancellation. */
  cancelDownload(): Promise<void>
  /** Subscribe to progress. @param receive - retained owner callback. @returns listener cleanup. */
  onDownloadProgress(receive: (state: NativeToolsDownloadState) => void): () => void
  /** Read current availability and user preferences. @returns the installed catalog. */
  listTools(): Promise<NativeToolCatalog>
  /**
   * Request startup of a fixed catalog entry; arbitrary paths and arguments are unsupported.
   * @param id - installed tool identity.
   * @param variant - the optional x32 debugger entry.
   * @returns whether the platform accepted the launch request, with a user-readable failure if rejected.
   */
  launchTool(id: NativeToolId, variant?: 'x32'): Promise<NativeToolLaunchResult>
  /** Save the complete favorites list. @param ids - selected tools. @returns durable completion. */
  setFavorites(ids: readonly NativeToolId[]): Promise<void>
}
