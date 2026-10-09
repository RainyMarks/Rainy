/** User-level tool directory state; native operations never enter a chat transcript. */
import { createStore, type Store } from '../ui/store.ts'
import type { NativeToolCatalog, NativeToolId, NativeToolsBridge, NativeToolsDownloadState, NativeToolsOperation, NativeToolsUpdateState } from '../../shared/native-tools-protocol.ts'

/** Retained directory state and in-flight operations. */
export interface NativeToolsState extends NativeToolCatalog {
  readonly phase: 'desktop-only' | 'idle' | 'loading' | 'ready' | 'error'
  readonly error: string
  readonly pending: readonly NativeToolId[]
  readonly savingFavorites: boolean
  readonly download: NativeToolsDownloadState
  readonly update: NativeToolsUpdateState
}

/** Localized outcomes are supplied by the registered locale owner. */
export interface NativeToolsCopy {
  readonly opened: (name: string) => string
  readonly launchFailed: (name: string) => string
  readonly favoritesFailed: () => string
  readonly favoriteSaved: (selected: boolean) => string
  readonly completed: (operation: NativeToolsOperation) => string
  readonly downloadFailed: () => string
}

/** Owns requests independently of a visible catalog panel. */
export class NativeToolsController {
  readonly state: Store<NativeToolsState>
  private disposed = false
  private loading: Promise<void> | undefined
  private downloading: Promise<void> | undefined
  private readonly unsubscribe: (() => void) | undefined
  private checking: Promise<void> | undefined
  private checked = false

  /**
   * @param bridge - desktop preload, absent in a browser-only deployment.
   * @param copy - locale-aware outcome text.
   * @param toast - retained shell feedback owner.
   */
  constructor(private readonly bridge: NativeToolsBridge | undefined, private readonly copy: NativeToolsCopy,
    private readonly toast: (message: string, kind: 'success' | 'error' | 'warning') => void) {
    this.state = createStore<NativeToolsState>({ phase: bridge === undefined ? 'desktop-only' : 'idle',
      tools: [], preferences: { favorites: [], recent: [] }, catalogOutdated: false, updateBytes: 0, error: '', pending: [], savingFavorites: false,
      download: { phase: 'idle', completedBytes: 0, totalBytes: 0, error: '' },
      update: { phase: 'unchecked', version: '', error: '' } })
    this.unsubscribe = bridge?.onDownloadProgress((download) => {
      if (!this.disposed) this.state.set({ ...this.state.getSnapshot(), download })
    })
  }

  /** Read current availability without discarding a populated catalog. @returns settled load. */
  load(): Promise<void> {
    if (this.disposed || this.bridge === undefined) return Promise.resolve()
    if (this.loading !== undefined) return this.loading
    const current = this.state.getSnapshot()
    if (current.pending.length > 0 || current.savingFavorites) return Promise.resolve()
    this.state.set({ ...this.state.getSnapshot(), phase: 'loading', error: '' })
    this.loading = this.read(this.bridge).finally(() => { this.loading = undefined })
    return this.loading
  }

  /**
   * Open one tool; overlapping requests for that tool are ignored.
   * @param id - selected catalog identity.
   * @param variant - optional x32 entry.
   * @returns completion of the launch request.
   */
  async launch(id: NativeToolId, variant?: 'x32'): Promise<void> {
    const before = this.state.getSnapshot()
    const tool = before.tools.find(entry => entry.id === id)
    if (this.isDisposed() || this.bridge === undefined || tool === undefined || before.pending.includes(id) || before.phase === 'loading') return
    const availability = variant === undefined ? tool.status : tool.variants?.[0]?.status
    if (availability !== 'ready') return
    this.state.set({ ...before, pending: [...before.pending, id] })
    try {
      const result = await this.bridge.launchTool(id, variant)
      if (this.disposed) return
      if (result.ok) {
        const current = this.state.getSnapshot()
        this.state.set({ ...current, preferences: { ...current.preferences,
          recent: [id, ...current.preferences.recent.filter(entry => entry !== id)] } })
        this.toast(result.warning ?? this.copy.opened(tool.name), result.warning === undefined ? 'success' : 'warning')
      } else {
        this.toast(result.error ?? this.copy.launchFailed(tool.name), 'error')
      }
    } catch (error) {
      if (!this.disposed) this.toast(error instanceof Error ? error.message : this.copy.launchFailed(tool.name), 'error')
    } finally {
      if (!this.disposed) {
        const current = this.state.getSnapshot()
        this.state.set({ ...current, pending: current.pending.filter(entry => entry !== id) })
      }
    }
  }

  /** Toggle a favorite after its durable write completes. @param id - catalog identity. @returns saved completion. */
  async toggleFavorite(id: NativeToolId): Promise<void> {
    const before = this.state.getSnapshot()
    if (this.isDisposed() || this.bridge === undefined || before.savingFavorites || before.phase === 'loading' || !before.tools.some(tool => tool.id === id)) return
    const selected = !before.preferences.favorites.includes(id)
    const favorites = selected ? [...before.preferences.favorites, id] : before.preferences.favorites.filter(entry => entry !== id)
    this.state.set({ ...before, savingFavorites: true })
    try {
      await this.bridge.setFavorites(favorites)
      if (this.disposed) return
      const current = this.state.getSnapshot()
      this.state.set({ ...current, preferences: { ...current.preferences, favorites } })
      this.toast(this.copy.favoriteSaved(selected), 'success')
    } catch (error) {
      if (!this.disposed) this.toast(error instanceof Error ? error.message : this.copy.favoritesFailed(), 'error')
    } finally {
      if (!this.disposed) this.state.set({ ...this.state.getSnapshot(), savingFavorites: false })
    }
  }

  /** Suppress late IPC responses and feedback after the owner is disposed. */
  dispose(): void { this.disposed = true; this.unsubscribe?.() }

  /**
   * Run one tool operation, then refresh availability; a second request while one runs is ignored.
   * @param operation - add tools, update installed tools, remove one tool, or repair damaged files.
   * @param ids - tools added or removed.
   * @returns settled operation and refresh.
   */
  operate(operation: NativeToolsOperation, ids: readonly NativeToolId[] = []): Promise<void> {
    if (this.disposed || !this.bridge) return Promise.resolve()
    if (this.downloading) return this.downloading
    const bridge = this.bridge
    const [removed] = ids
    if (operation === 'remove' && removed === undefined) return Promise.resolve()
    this.state.set({ ...this.state.getSnapshot(), download: { phase: operation === 'repair' ? 'verifying' : operation === 'remove' ? 'installing' : 'downloading',
      completedBytes: 0, totalBytes: 0, error: '', operation, tools: ids } })
    this.downloading = (async () => {
      try {
        if (operation === 'remove' && removed !== undefined) await bridge.removeTool(removed)
        else if (operation === 'repair') await bridge.repairTools()
        else await bridge.installTools(operation === 'update' ? [] : ids)
        if (this.disposed) return
        const download = await bridge.getDownloadState()
        this.state.set({ ...this.state.getSnapshot(), download })
        if (download.phase === 'complete') {
          await this.load()
          this.toast(this.copy.completed(operation), 'success')
        }
      } catch (error) {
        if (this.disposed) return
        const message = error instanceof Error ? error.message : this.copy.downloadFailed()
        this.state.set({ ...this.state.getSnapshot(), download: { ...this.state.getSnapshot().download, phase: 'error', error: message } })
        this.toast(message, 'error')
      }
    })().finally(() => { this.downloading = undefined })
    return this.downloading
  }

  /** Safely stop the current download without discarding retained state. @returns cancellation completion. */
  async cancelDownload(): Promise<void> {
    if (this.disposed || !this.bridge) return
    try { await this.bridge.cancelDownload() }
    catch (error) { if (!this.isDisposed()) this.toast(error instanceof Error ? error.message : this.copy.downloadFailed(), 'error') }
  }

  /** Check for publisher additions and updates while retaining the current catalog. @returns settled check. */
  checkUpdates(): Promise<void> {
    if (this.disposed || !this.bridge) return Promise.resolve()
    if (this.checking) return this.checking
    this.state.set({ ...this.state.getSnapshot(), update: { ...this.state.getSnapshot().update, phase: 'checking', error: '' } })
    const bridge = this.bridge
    this.checking = (async () => {
      try {
        const update = await bridge.checkToolUpdates()
        const download = await bridge.getDownloadState()
        if (this.disposed) return
        this.state.set({ ...this.state.getSnapshot(), update, download })
      } catch (error) {
        if (!this.disposed) this.state.set({ ...this.state.getSnapshot(), update: { phase: 'error', version: '', error: error instanceof Error ? error.message : this.copy.downloadFailed() } })
      }
    })().finally(() => { this.checking = undefined })
    return this.checking
  }

  private isDisposed(): boolean { return this.disposed }

  private async read(bridge: NativeToolsBridge): Promise<void> {
    try {
      const [catalog, download] = await Promise.all([bridge.listTools(), bridge.getDownloadState()])
      if (this.disposed) return
      const current = this.state.getSnapshot()
      // A checked channel stays authoritative; whether installed tools still differ from it follows the new listing.
      const outdated = catalog.catalogOutdated || catalog.tools.some(tool => tool.outdated)
      const update = current.update.phase === 'available' || current.update.phase === 'current'
        ? { ...current.update, phase: outdated ? 'available' as const : 'current' as const } : current.update
      this.state.set({ ...current, ...catalog, download, update, phase: 'ready', error: '' })
      if (!this.checked) { this.checked = true; void this.checkUpdates() }
    } catch (error) {
      if (!this.disposed) this.state.set({ ...this.state.getSnapshot(), phase: 'error', error: error instanceof Error ? error.message : '' })
    }
  }
}
