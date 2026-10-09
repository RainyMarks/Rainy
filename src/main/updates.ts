/** Stable GitHub updates download in the background and install only after saved shutdown. */
import type { AppUpdater, ProgressInfo, UpdateCheckResult, UpdateDownloadedEvent, UpdateInfo } from 'electron-updater'

interface UpdateEvents {
  error: (error: Error) => void
  'download-progress': (progress: ProgressInfo) => void
  'update-downloaded': (info: UpdateDownloadedEvent) => void
}
type CheckedUpdate = Pick<UpdateCheckResult, 'isUpdateAvailable'> & { readonly updateInfo: Pick<UpdateInfo, 'version'> }

/** Maintained updater operations used by the Rainy desktop carrier. */
export type RainyUpdater = Pick<AppUpdater, 'autoDownload' | 'autoInstallOnAppQuit' | 'allowPrerelease' | 'allowDowngrade'
  | 'channel' | 'setFeedURL' | 'downloadUpdate' | 'quitAndInstall'> & {
    checkForUpdates(): Promise<CheckedUpdate | null>
    on<K extends keyof UpdateEvents>(event: K, listener: UpdateEvents[K]): unknown
    off<K extends keyof UpdateEvents>(event: K, listener: UpdateEvents[K]): unknown
  }

/** Main-process update state; renderer input never supplies an installer or release URL. */
export type RainyUpdateState =
  | { readonly phase: 'idle' | 'checking' | 'current' | 'unavailable' }
  | { readonly phase: 'downloading'; readonly version: string; readonly percent: number }
  | { readonly phase: 'ready' | 'installing'; readonly version: string }
  | { readonly phase: 'error'; readonly operation: 'check' | 'download' | 'install'; readonly message: string; readonly version?: string }

/** Native notices, installation consent, and the application's saved-shutdown owner. */
export interface RainyUpdateHooks {
  readonly enabled: boolean
  /** Observe progress; automatic checks remain silent when manual is false.
   * @param state Latest updater outcome or progress.
   * @param manual Whether the current operation needs user-requested feedback.
   * @returns Optional native-notice completion; failures are contained by the controller.
   */
  notice(state: RainyUpdateState, manual: boolean): void | Promise<void>
  /** Offer later or restart-and-install for this prepared release.
   * @param version Prepared release shown in the confirmation.
   * @returns Whether the user chose installation now.
   */
  confirm(version: string): Promise<boolean>
  /** Save drafts and stop owned processes before invoking install; false preserves the running app.
   * @param install Single-use installation handoff, valid only until this callback settles.
   * @returns Whether saved shutdown handed off to the installer.
   */
  restartWithInstall(install: () => void): Promise<boolean>
}

/** Serialize update preparation and confirmed installation. */
export class RainyUpdates {
  private current: RainyUpdateState = { phase: 'idle' }
  private operation: Promise<RainyUpdateState> | undefined
  private candidate: string | undefined
  private downloaded = false
  private disposed = false
  private manual = false

  private readonly onProgress = (progress: ProgressInfo): void => {
    if (this.current.phase !== 'downloading' || this.candidate === undefined) return
    this.publish({ phase: 'downloading', version: this.candidate, percent: Math.min(100, Math.max(0, Math.round(progress.percent))) })
  }

  private readonly onDownloaded = (info: UpdateDownloadedEvent): void => {
    if (this.current.phase === 'downloading' && info.version === this.candidate) this.downloaded = true
  }

  private readonly onError = (error: Error): void => {
    // Check and download promises report their own failures; installation can fail after handoff.
    if (this.current.phase === 'installing') this.failure('install', error)
  }

  /**
   * @param updater Process-owned electron-updater instance; no Electron runtime is imported here.
   * @param hooks Native UI and saved-shutdown callbacks.
   */
  constructor(private readonly updater: RainyUpdater, private readonly hooks: RainyUpdateHooks) {
    updater.setFeedURL({ provider: 'github', owner: 'RainyMarks', repo: 'RainyAgent' })
    updater.autoDownload = false
    updater.autoInstallOnAppQuit = false
    updater.channel = 'latest'
    updater.allowPrerelease = false
    updater.allowDowngrade = false
    updater.on('download-progress', this.onProgress)
    updater.on('update-downloaded', this.onDownloaded)
    updater.on('error', this.onError)
  }

  /** Latest state for native menu labels and progress. */
  get state(): RainyUpdateState { return this.current }

  /**
   * Check and download once; a manual request joins pending work or revisits a prepared update.
   * @param manual Whether the user requested feedback for this operation.
   * @returns Settled update state, including recoverable failures and a declined installation.
   */
  check(manual = false): Promise<RainyUpdateState> {
    if (this.disposed) return Promise.resolve(this.current)
    if (this.operation !== undefined) {
      if (manual && !this.manual) { this.manual = true; this.report() }
      return this.operation
    }
    if (this.current.phase === 'installing') return Promise.resolve(this.current)
    if (this.downloaded && !manual) return Promise.resolve(this.current)
    this.manual = manual
    this.operation = Promise.resolve().then(() => this.run()).finally(() => { this.operation = undefined })
    return this.operation
  }

  /** Stop UI callbacks immediately and retain the error listener until pending updater work settles. */
  dispose(): void {
    if (this.disposed) return
    this.disposed = true
    this.updater.off('download-progress', this.onProgress)
    this.updater.off('update-downloaded', this.onDownloaded)
    void Promise.allSettled([this.operation]).then(() => { this.updater.off('error', this.onError) })
  }

  private async run(): Promise<RainyUpdateState> {
    if (this.isDisposed()) return this.current
    if (!this.hooks.enabled) return this.publish({ phase: 'unavailable' })
    if (this.preparedVersion() !== undefined) return this.offerInstall()
    this.candidate = undefined
    this.publish({ phase: 'checking' })
    try {
      const result = await this.updater.checkForUpdates()
      if (this.isDisposed()) return this.current
      if (result === null) return this.failure('check', new Error('The installed app has no update source.'))
      if (!result.isUpdateAvailable) return this.publish({ phase: 'current' })
      this.candidate = result.updateInfo.version
    } catch (error) { return this.failure('check', error) }
    this.publish({ phase: 'downloading', version: this.candidate, percent: 0 })
    try {
      await this.updater.downloadUpdate()
      if (this.isDisposed()) return this.current
      if (this.preparedVersion() !== this.candidate) throw new Error('The update download did not prepare an installer.')
    } catch (error) {
      this.downloaded = false
      return this.failure('download', error)
    }
    this.publish({ phase: 'ready', version: this.candidate })
    return this.offerInstall()
  }

  private async offerInstall(): Promise<RainyUpdateState> {
    const version = this.preparedVersion()
    if (this.isDisposed() || version === undefined) return this.current
    try {
      const confirmed = await this.hooks.confirm(version)
      if (this.isDisposed() || !confirmed) return this.current
      this.manual = true
      this.publish({ phase: 'installing', version })
      const handoff = { accepting: true, started: false }
      let restarted: boolean
      try {
        restarted = await this.hooks.restartWithInstall(() => {
          if (!handoff.accepting || this.isDisposed() || handoff.started) return
          handoff.started = true
          this.updater.quitAndInstall(true, true)
        })
      } finally { handoff.accepting = false }
      if (this.isDisposed() || handoff.started) return this.current
      if (restarted) throw new Error('Saved shutdown completed without starting the prepared installer.')
      return this.publish({ phase: 'ready', version })
    } catch (error) { return this.failure('install', error) }
  }

  private isDisposed(): boolean { return this.disposed }

  private preparedVersion(): string | undefined { return this.downloaded ? this.candidate : undefined }

  private failure(operation: 'check' | 'download' | 'install', error: unknown): RainyUpdateState {
    return this.publish({ phase: 'error', operation, message: error instanceof Error ? error.message : String(error),
      ...(this.candidate === undefined ? {} : { version: this.candidate }) })
  }

  private publish(state: RainyUpdateState): RainyUpdateState {
    if (!this.disposed) { this.current = state; this.report() }
    return this.current
  }

  private report(): void {
    if (this.disposed) return
    const failed = (error: unknown): void => { console.error('RainyAgent update notice failed:', error) }
    try { void Promise.resolve(this.hooks.notice(this.current, this.manual)).catch(failed) }
    catch (error) { failed(error) }
  }
}
