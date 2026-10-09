/** Native reload admission while the renderer and Host can still save workbench drafts. */
import type { Input } from 'electron'

/** Renderer acknowledgement shared by application exit and native reload. */
export interface DraftFlushResult {
  readonly ok: boolean
  readonly error?: string
}

/** Callbacks owned by the carrier window and its existing save bridge. */
export interface SavedReloadOptions {
  /** Save current drafts. @returns the durable-save acknowledgement. */
  readonly flush: () => Promise<DraftFlushResult>
  /** Reload the current document. @param ignoreCache - whether to bypass the browser cache. */
  readonly reload: (ignoreCache: boolean) => void
  /** Check whether exit or window destruction prevents reload. @returns whether reload must stop. */
  readonly isClosing: () => boolean
  /** Keep the document open and report a failed save. @param error - save failure details. @returns completion of the notice. */
  readonly reportFailure: (error: string) => Promise<void>
}

/**
 * Coalesce native reload requests and preserve the document after a failed save.
 * @param options - window operations and its shared durable-save callback.
 * @returns a reload callback that waits for saving and any failure notice.
 */
export function createSavedReload(options: SavedReloadOptions): (ignoreCache?: boolean) => Promise<void> {
  let pending: Promise<void> | undefined
  return (ignoreCache = false) => {
    if (pending !== undefined) return pending
    if (options.isClosing()) return Promise.resolve()
    pending = (async () => {
      let saved: DraftFlushResult
      try { saved = await options.flush() }
      catch (error) { saved = { ok: false, error: error instanceof Error ? error.message : String(error) } }
      if (options.isClosing()) return
      if (saved.ok) options.reload(ignoreCache)
      else await options.reportFailure(saved.error ?? '工具未能确认草稿已保存，请重试。')
    })().finally(() => { pending = undefined })
    return pending
  }
}

/**
 * Identify browser reload shortcuts before Electron dispatches them to a frame or menu.
 * @param input - native key event.
 * @returns whether to bypass cache, or undefined for a different input.
 */
export function nativeReloadShortcut(input: Pick<Input, 'type' | 'key' | 'control' | 'meta' | 'shift' | 'alt'>): boolean | undefined {
  if (input.type !== 'keyDown' || input.alt) return undefined
  if (input.key === 'F5') return input.shift || input.control || input.meta
  if (input.key.toLowerCase() === 'r' && (input.control || input.meta)) return input.shift
  return undefined
}
