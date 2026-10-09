/** Native Strata status and the user's explicit operations for the Models section. */
import { useSyncExternalStore } from 'react'
import { z } from 'zod'
import type { ModelSetup } from '../../shared/rpc.ts'
import { strataStatusSchema } from '../../shared/strata-protocol.ts'
import type { StrataModelPicker, StrataNativeHost, StrataSettings, StrataStatus } from '../../shared/strata-protocol.ts'
import { errorText } from './parts.tsx'

type Pending = 'save' | 'start' | 'stop' | 'choose' | 'connect'

/** Status kept across reads; operation failures go to the notifier instead. */
export interface StrataSnapshot {
  readonly available: boolean
  readonly status?: StrataStatus | undefined
  readonly loading: boolean
  readonly pending?: Pending | undefined
  readonly error: string
}

/** Operations offered by the Strata card. */
export interface StrataActions {
  strataRefresh(this: void): Promise<void>
  strataSave(this: void, settings: StrataSettings): Promise<StrataStatus | undefined>
  strataStart(this: void): Promise<void>
  strataStop(this: void): Promise<void>
  strataChoose(this: void, kind: StrataModelPicker): Promise<string | null>
  strataConnect(this: void): Promise<ModelSetup | undefined>
}

/** Completion texts, read when each operation finishes so they follow the current locale. */
export interface StrataCopy { saved(): string; stopped(): string; connected(): string }
type Selection = Awaited<ReturnType<StrataNativeHost['connect']>>
const selectionSchema = z.object({ provider: z.string(), model: z.string() }).strict()

/** Owns native request lifetimes; it never starts a model or selects an endpoint on its own. */
export class StrataController {
  readonly actions: StrataActions
  private snapshot: StrataSnapshot
  private readonly listeners = new Set<() => void>()
  private reading: Promise<void> | undefined
  private revision = 0
  private disposed = false

  /**
   * @param bridge Desktop methods; `undefined` in a plain browser.
   * @param copy Completion texts.
   * @param notify Shows an outcome; `success` marks a completed operation.
   * @param configured Reads the saved model after the desktop connected Strata through the Host.
   */
  constructor(private readonly bridge: StrataNativeHost | undefined, private readonly copy: StrataCopy,
    private readonly notify: (message: string, success?: boolean) => void,
    private readonly configured: (selection: Selection) => Promise<ModelSetup>) {
    this.snapshot = { available: bridge !== undefined, loading: false, error: '' }
    this.actions = {
      strataRefresh: () => this.refresh(),
      strataSave: settings => this.perform('save', async (native) => {
        const status = strataStatusSchema.parse(await native.save(settings))
        return { value: status, status, message: this.copy.saved() }
      }),
      strataStart: async () => { await this.perform('start', async (native) => {
        const status = strataStatusSchema.parse(await native.start())
        return { value: undefined, status }
      }) },
      strataStop: async () => { await this.perform('stop', async (native) => {
        const status = strataStatusSchema.parse(await native.stop())
        return { value: undefined, status, ...(status.phase === 'stopped' ? { message: this.copy.stopped() } : {}) }
      }) },
      strataChoose: async kind => (await this.perform('choose', async native => ({ value: z.string().nullable().parse(await native.selectModel(kind)) }))) ?? null,
      strataConnect: () => this.perform('connect', async (native) => {
        const selected = selectionSchema.parse(await native.connect())
        if (this.disposed) return { value: undefined }
        return { value: await this.configured(selected), message: this.copy.connected() }
      }),
    }
  }

  /** @returns The current snapshot. */
  getSnapshot = (): StrataSnapshot => this.snapshot

  /**
   * @param listener Called after each snapshot change.
   * @returns A function that removes the listener.
   */
  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /** @returns After the status read; concurrent reads share one request, and a read superseded by an operation is dropped. */
  refresh(): Promise<void> {
    if (this.disposed || this.bridge === undefined || this.snapshot.pending !== undefined) return Promise.resolve()
    if (this.reading !== undefined) return this.reading
    const revision = this.revision
    this.set({ loading: true, error: '' })
    this.reading = this.bridge.status().then((value) => {
      const status = strataStatusSchema.parse(value)
      if (!this.disposed && revision === this.revision) this.set({ status, error: '' })
    }).catch((error: unknown) => {
      if (!this.disposed && revision === this.revision) this.set({ error: errorText(error) })
    }).finally(() => {
      this.reading = undefined
      if (!this.disposed) this.set({ loading: false })
    })
    return this.reading
  }

  /** Ignore late native completions after the card unmounts; the desktop keeps owning the engine process. */
  dispose(): void { this.disposed = true; this.revision++ }

  private set(change: Partial<StrataSnapshot>): void {
    this.snapshot = { ...this.snapshot, ...change }
    for (const listener of this.listeners) listener()
  }

  private async perform<T>(pending: Pending,
    operation: (bridge: StrataNativeHost) => Promise<{ value: T; status?: StrataStatus; message?: string }>): Promise<T | undefined> {
    const previous = this.snapshot.pending
    if (this.disposed || this.bridge === undefined || (previous !== undefined && !(pending === 'stop' && previous === 'start'))) return undefined
    const revision = ++this.revision
    this.set({ pending })
    try {
      const result = await operation(this.bridge)
      if (this.disposed || revision !== this.revision) return undefined
      if (result.status !== undefined) this.set({ status: result.status, error: '' })
      if (result.message !== undefined) this.notify(result.message, true)
      return result.value
    } catch (error) {
      if (!this.disposed && revision === this.revision) this.notify(errorText(error))
      return undefined
    } finally {
      if (!this.disposed && revision === this.revision) this.set({ pending: undefined })
    }
  }
}

/**
 * @param controller The card's controller.
 * @returns Its snapshot; re-renders on change.
 */
export function useStrata(controller: StrataController): StrataSnapshot {
  return useSyncExternalStore(controller.subscribe, controller.getSnapshot)
}
