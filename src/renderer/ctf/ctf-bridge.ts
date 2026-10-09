/** Configuration and acknowledged saves for one retained workbench iframe. */
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import { randomUUID } from '@deepseek-ai/dsh-util-crypto'
import type { CtfConfiguration, CtfFlushResult, CtfHostMessage } from './ctf-protocol.ts'

/** Host view state includes failures before the frame's application is ready. */
export interface CtfWorkbenchState {
  readonly phase: 'loading' | 'ready' | 'error'
  readonly error: 'load' | 'timeout' | 'save' | undefined
  readonly message: string
  readonly saving: 'idle' | 'saving' | 'saved' | 'error'
}

/** Validated deadlines and host feedback callbacks. */
export interface CtfBridgeOptions {
  readonly origin: string
  readonly readyTimeoutMs: number
  readonly flushTimeoutMs: number
  readonly toast: (message: string, kind: 'success' | 'error' | 'warning') => void
  readonly flushFailureMessage: () => string
}

interface PendingFlush {
  readonly timer: ReturnType<typeof setTimeout>
  readonly resolve: (result: CtfFlushResult) => void
}

/** One frame's transport; draft and API contents stay outside its messages. */
export class CtfWorkbenchBridge {
  readonly state = createSnapshotStore<CtfWorkbenchState>({ phase: 'loading', error: undefined, message: '', saving: 'idle' })
  private frame: HTMLIFrameElement | null = null
  private configuration: CtfConfiguration | undefined
  private revision = 0
  private ready = false
  private configured = false
  private disposed = false
  private readinessTimer: ReturnType<typeof setTimeout> | undefined
  private readonly pending = new Map<string, PendingFlush>()

  /** @param options - exact origin, deadlines, and user feedback callbacks. */
  constructor(private readonly options: CtfBridgeOptions) {}

  /** Attach the retained frame. @param frame - current carrier. */
  attach(frame: HTMLIFrameElement | null): void {
    this.frame = frame
    if (frame !== null) this.waitForReady()
  }

  /** Publish context and appearance without replacing the document. @param configuration - current workspace. */
  configure(configuration: CtfConfiguration): void {
    const previous = this.configuration
    this.configuration = configuration
    this.revision++
    if (this.disposed || !this.ready) return
    if (previous?.context.kind !== configuration.context.kind
      || (previous.context.kind === 'session' && configuration.context.kind === 'session' && previous.context.id !== configuration.context.id)) {
      this.state.set({ ...this.state.getSnapshot(), phase: 'loading', error: undefined, message: '' })
      this.waitForReady()
    }
    this.sendConfiguration()
  }

  /** Accept only the current frame at the exact local origin. @param event - browser message. */
  receive(event: MessageEvent<unknown>): void {
    if (this.disposed || event.origin !== this.options.origin || event.source !== this.frame?.contentWindow) return
    const data = event.data
    if (data === null || typeof data !== 'object' || !('type' in data)) return
    switch (data.type) {
      case 'rainy:ready':
        this.ready = true
        this.sendConfiguration()
        return
      case 'rainy:loaded':
        if (!('revision' in data) || data.revision !== this.revision) return
        this.clearReadinessTimer()
        this.state.set({ ...this.state.getSnapshot(), phase: 'ready', error: undefined, message: '' })
        return
      case 'rainy:error':
        if (!('message' in data) || typeof data.message !== 'string') return
        this.clearReadinessTimer()
        this.state.set({ ...this.state.getSnapshot(), phase: 'error', error: 'load', message: data.message })
        return
      case 'rainy:flushed': {
        if (!('id' in data) || typeof data.id !== 'string' || !('ok' in data) || typeof data.ok !== 'boolean') return
        const pending = this.pending.get(data.id)
        if (pending === undefined) return
        this.pending.delete(data.id)
        clearTimeout(pending.timer)
        pending.resolve({ ok: data.ok, ...'error' in data && typeof data.error === 'string' ? { error: data.error } : {} })
        return
      }
      case 'rainy:status':
        if (!('state' in data) || (data.state !== 'saving' && data.state !== 'saved' && data.state !== 'error')) return
        this.state.set({ ...this.state.getSnapshot(), saving: data.state,
          message: 'message' in data && typeof data.message === 'string' ? data.message : '' })
        return
      case 'rainy:toast':
        if ('message' in data && typeof data.message === 'string' && 'kind' in data
          && (data.kind === 'success' || data.kind === 'error' || data.kind === 'warning')) this.options.toast(data.message, data.kind)
        return
      default:
        return
    }
  }

  /** Report a carrier load failure without clearing the frame. */
  loadFailed(): void {
    this.clearReadinessTimer()
    this.state.set({ ...this.state.getSnapshot(), phase: 'error', error: 'load', message: '' })
  }

  /** Flush before closing or replacing the document. @returns save acknowledgement. */
  flush(): Promise<CtfFlushResult> {
    if (!this.configured && !this.disposed) return Promise.resolve({ ok: true })
    if (this.disposed || !this.ready || this.frame?.contentWindow === null || this.frame === null) {
      return Promise.resolve({ ok: false, error: this.options.flushFailureMessage() })
    }
    const id = randomUUID()
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id)
        resolve({ ok: false, error: this.options.flushFailureMessage() })
      }, this.options.flushTimeoutMs)
      this.pending.set(id, { timer, resolve })
      this.post({ type: 'rainy:flush', id })
    })
  }

  /** Reload only after confirmed saves. @returns whether reload was requested. */
  async retry(): Promise<boolean> {
    const before = this.state.getSnapshot()
    const saveOnly = before.saving === 'error' && (before.phase === 'ready' || before.error === 'save')
    const saved = await this.flush()
    if (this.disposed) return false
    if (!saved.ok) {
      this.state.set({ ...this.state.getSnapshot(), phase: 'error', error: 'save', message: saved.error ?? '' })
      return false
    }
    if (saveOnly) {
      this.state.set({ ...this.state.getSnapshot(), phase: 'ready', error: undefined, message: '', saving: 'saved' })
      return true
    }
    this.ready = false
    this.state.set({ phase: 'loading', error: undefined, message: '', saving: 'idle' })
    this.waitForReady()
    if (this.frame !== null) this.frame.src = this.frame.src
    return true
  }

  /** Settle pending waits and prevent disposed frames from publishing feedback. */
  dispose(): void {
    this.disposed = true
    this.clearReadinessTimer()
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer)
      pending.resolve({ ok: false, error: this.options.flushFailureMessage() })
    }
    this.pending.clear()
    this.frame = null
  }

  private sendConfiguration(): void {
    if (this.configuration === undefined) return
    this.configured = true
    this.post({ type: 'rainy:configure', revision: this.revision, ...this.configuration })
  }

  private post(message: CtfHostMessage): void {
    this.frame?.contentWindow?.postMessage(message, this.options.origin)
  }

  private waitForReady(): void {
    this.clearReadinessTimer()
    this.readinessTimer = setTimeout(() => {
      this.state.set({ ...this.state.getSnapshot(), phase: 'error', error: 'timeout', message: '' })
    }, this.options.readyTimeoutMs)
  }

  private clearReadinessTimer(): void {
    if (this.readinessTimer !== undefined) clearTimeout(this.readinessTimer)
    this.readinessTimer = undefined
  }
}
