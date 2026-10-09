/** Retained native Strata status and explicit user operations for the model settings card. */
import { z } from 'zod'
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import { strataStatusSchema } from '../strata-protocol.ts'
import type { StrataModelPicker, StrataNativeHost, StrataSettings, StrataStatus } from '../strata-protocol.ts'
import type { RainyModelSetup } from './settings-protocol.ts'

type Pending = 'save' | 'start' | 'stop' | 'choose' | 'connect'
/** The status request retains existing content; operation failures use the app-wide notification owner. */
export interface StrataSnapshot {
  readonly available: boolean
  readonly status?: StrataStatus | undefined
  readonly loading: boolean
  readonly pending?: Pending | undefined
  readonly error: string
}

/** Plain callbacks exposed to the model settings section. */
export interface StrataActions {
  strataRefresh(this: void): Promise<void>
  strataSave(this: void, settings: StrataSettings): Promise<StrataStatus | undefined>
  strataStart(this: void): Promise<void>
  strataStop(this: void): Promise<void>
  strataChoose(this: void, kind: StrataModelPicker): Promise<string | null>
  strataConnect(this: void): Promise<RainyModelSetup | undefined>
}

interface Copy { saved(): string; stopped(): string; connected(): string }
type Selection = Awaited<ReturnType<StrataNativeHost['connect']>>
const selectionSchema = z.object({ provider: z.string(), model: z.string() }).strict()

/** Own native request lifetimes without starting a model or selecting an endpoint implicitly. */
export class StrataController {
  readonly state = createSnapshotStore<StrataSnapshot>({ available: false, loading: false, error: '' })
  readonly actions: StrataActions
  private reading: Promise<void> | undefined
  private revision = 0
  private disposed = false

  /**
   * @param bridge Context-isolated desktop methods, absent in a browser-only deployment.
   * @param copy Locale-following completion messages.
   * @param notify App-wide transient feedback owner.
   * @param configured Read the saved model after the native Host connection succeeds.
   */
  constructor(private readonly bridge: StrataNativeHost | undefined, private readonly copy: Copy,
    private readonly notify: (message: string, success?: boolean) => void,
    private readonly configured: (selection: Selection) => Promise<RainyModelSetup>) {
    this.state.set({ available: bridge !== undefined, loading: false, error: '' })
    this.actions = {
      strataRefresh: () => this.refresh(),
      strataSave: settings => this.perform('save', async (host) => {
        const status = strataStatusSchema.parse(await host.save(settings))
        return { value: status, status, message: this.copy.saved() }
      }),
      strataStart: async () => { await this.perform('start', async (host) => {
        const status = strataStatusSchema.parse(await host.start())
        return { value: undefined, status }
      }) },
      strataStop: async () => { await this.perform('stop', async (host) => {
        const status = strataStatusSchema.parse(await host.stop())
        return { value: undefined, status, ...(status.phase === 'stopped' ? { message: this.copy.stopped() } : {}) }
      }) },
      strataChoose: async kind => (await this.perform('choose', async host => ({ value: z.string().nullable().parse(await host.selectModel(kind)) }))) ?? null,
      strataConnect: () => this.perform('connect', async (host) => {
        const selected = selectionSchema.parse(await host.connect())
        if (this.disposed) return { value: undefined }
        return { value: await this.configured(selected), message: this.copy.connected() }
      }),
    }
  }

  /** Coalesce visible status polls and discard reads superseded by a user operation. @returns Settled read. */
  refresh(): Promise<void> {
    if (this.disposed || this.bridge === undefined || this.state.getSnapshot().pending !== undefined) return Promise.resolve()
    if (this.reading !== undefined) return this.reading
    const revision = this.revision
    this.state.set({ ...this.state.getSnapshot(), loading: true, error: '' })
    this.reading = this.bridge.status().then((value) => {
      const status = strataStatusSchema.parse(value)
      if (!this.disposed && revision === this.revision) this.state.set({ ...this.state.getSnapshot(), status, error: '' })
    }).catch((error: unknown) => {
      if (!this.disposed && revision === this.revision) this.state.set({ ...this.state.getSnapshot(), error: this.message(error) })
    }).finally(() => {
      this.reading = undefined
      if (!this.disposed) this.state.set({ ...this.state.getSnapshot(), loading: false })
    })
    return this.reading
  }

  /** Suppress late IPC completions after the client plugin unloads; the carrier owns process shutdown. */
  dispose(): void { this.disposed = true; this.revision++ }

  private async perform<T>(pending: Pending,
    operation: (bridge: StrataNativeHost) => Promise<{ value: T; status?: StrataStatus; message?: string }>): Promise<T | undefined> {
    const previous = this.state.getSnapshot().pending
    if (this.disposed || this.bridge === undefined || (previous !== undefined && !(pending === 'stop' && previous === 'start'))) return undefined
    const revision = ++this.revision
    this.state.set({ ...this.state.getSnapshot(), pending })
    try {
      const result = await operation(this.bridge)
      if (this.isDisposed() || revision !== this.revision) return undefined
      if (result.status !== undefined) this.state.set({ ...this.state.getSnapshot(), status: result.status, error: '' })
      if (result.message !== undefined) this.notify(result.message, true)
      return result.value
    } catch (error) {
      if (!this.isDisposed() && revision === this.revision) this.notify(this.message(error))
      return undefined
    } finally {
      if (!this.isDisposed() && revision === this.revision) this.state.set({ ...this.state.getSnapshot(), pending: undefined })
    }
  }

  private isDisposed(): boolean { return this.disposed }

  private message(error: unknown): string { return error instanceof Error ? error.message : String(error) }
}
