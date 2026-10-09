/** Optional desktop components that are downloaded when first needed instead of shipping in the installer. */

/** Components the user can download or remove from settings. */
export type OptionalModuleId = 'strata' | 'php'

/** Installation state of one component; sizes come from the signed carrier resources. */
export interface OptionalModuleStatus {
  readonly id: OptionalModuleId
  readonly installed: boolean
  /** Compressed download size in bytes. */
  readonly downloadBytes: number
  /** Disk space used after installation, in bytes. */
  readonly unpackedBytes: number
}

/** Progress of the latest component download. */
export interface OptionalModulesState {
  readonly phase: 'idle' | 'downloading' | 'installing' | 'complete' | 'cancelled' | 'error'
  readonly module?: OptionalModuleId
  readonly completedBytes: number
  readonly totalBytes: number
  readonly error: string
}

/** Preload operations available only to the trusted main window. */
export interface OptionalModulesBridge {
  /** @returns every downloadable component and whether it is installed. */
  list(): Promise<readonly OptionalModuleStatus[]>
  /** @returns progress of the latest download, for a page opened while it runs. */
  state(): Promise<OptionalModulesState>
  /** Download, verify and unpack one component. @param id - component. @returns completion. */
  install(id: OptionalModuleId): Promise<void>
  /** Delete an installed component. @param id - component. @returns completion. */
  remove(id: OptionalModuleId): Promise<void>
  /** Stop the running download; downloaded pieces are kept for the next attempt. @returns settled cancellation. */
  cancel(): Promise<void>
  /** @param receive - progress observer. @returns listener cleanup. */
  onProgress(receive: (state: OptionalModulesState) => void): () => void
}
