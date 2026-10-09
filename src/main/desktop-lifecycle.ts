/** Carrier-owned exit and execution-target transitions while drafts and the Host remain available. */
import type { DraftFlushResult } from './saved-reload.ts'

/** Result returned to either native target-selection entry point. */
export interface TargetSwitchResult {
  readonly ok: boolean
  readonly error?: string
}

/** An idle, frozen Host whose target preference has not yet been committed. */
export type PreparedTargetSwitch = { readonly ok: false; readonly error?: string } | {
  readonly ok: true
  /** Persist the selected target. @returns completion of the atomic preference update. */
  commit(): Promise<void>
  /** Release the Host freeze after cancellation or failure. @returns completion of the resume acknowledgement. */
  resume(): Promise<void>
}

/** Operations owned by the Electron carrier, in their required cleanup order. */
export interface DesktopLifecycleOptions {
  /** Save drafts before exit. @returns the durable-save acknowledgement. */
  flush(): Promise<DraftFlushResult>
  /** Offer a retry after saving fails. @param error - save failure details. @returns whether saving should retry. */
  retrySave(error: string | undefined): Promise<boolean>
  readonly cleanup: readonly (() => Promise<void>)[]
  /** Report failures after every cleanup step settles. @param error - all failed cleanup steps. */
  reportCleanupFailure(error: AggregateError): void
  /** Finish application exit after cleanup. */
  exit(): void
  /** Schedule relaunch and request exit with drafts already saved. */
  restart(): void
}

/** Admission and final exit callbacks owned by the first caller requesting exit. */
export interface DesktopQuitRequest {
  /** Check exit eligibility after target switching settles. @returns whether saving and cleanup may proceed. */
  prepare(): Promise<boolean>
  /** Finish this exit after draft saving and every cleanup step settles. */
  exit(): void
}

/** One carrier owns both native-menu and renderer requests. */
export interface DesktopLifecycle {
  /** Whether exit has been requested, including draft-save confirmation. */
  readonly closing: boolean
  /** Whether draft saving has succeeded and window destruction may proceed. */
  readonly quitting: boolean
  /** Whether target inspection or commit owns the carrier. */
  readonly switching: boolean
  /**
   * Coalesce exit requests and drain an admitted target switch before saving and cleanup.
   * @param draftsSaved - whether the requesting target switch already saved drafts.
   * @param request - optional admission and exit callbacks; later coalesced callers cannot replace them.
   * @returns completion after exit or cancelled admission/saving; admission and save failures may reject.
   */
  quit(draftsSaved?: boolean, request?: DesktopQuitRequest): Promise<void>
  /**
   * Admit one target switch; exit cancels preparation but waits for an admitted commit.
   * @param prepare - save drafts and inspect the project before freezing its idle Host.
   * @returns a rejection result for busy or closing state; preparation and commit failures reject.
   */
  switchTarget(prepare: () => Promise<PreparedTargetSwitch>): Promise<TargetSwitchResult>
}

const closingResult: TargetSwitchResult = { ok: false, error: '应用正在退出，执行环境保持不变。' }

/**
 * Keep a single owner for exit admission, target commits, and awaited cleanup.
 * @param options - carrier callbacks; cleanup entries run sequentially despite individual failures.
 * @returns the shared native-menu and IPC lifecycle controller.
 */
export function createDesktopLifecycle(options: DesktopLifecycleOptions): DesktopLifecycle {
  let quitting = false
  let pendingQuit: Promise<void> | undefined
  let pendingSwitch: Promise<TargetSwitchResult> | undefined
  const isClosing = (): boolean => quitting || pendingQuit !== undefined
  return {
    get closing() { return isClosing() },
    get quitting() { return quitting },
    get switching() { return pendingSwitch !== undefined },
    quit(draftsSaved = false, request) {
      if (pendingQuit !== undefined) return pendingQuit
      if (quitting) return Promise.resolve()
      pendingQuit = Promise.resolve().then(async () => {
        if (pendingSwitch !== undefined) await Promise.allSettled([pendingSwitch])
        if (request && !await request.prepare()) return
        for (; !draftsSaved;) {
          const saved = await options.flush()
          if (saved.ok) break
          if (!await options.retrySave(saved.error)) return
        }
        quitting = true
        const failures: unknown[] = []
        try {
          for (const cleanup of options.cleanup) {
            try { await cleanup() } catch (error) { failures.push(error) }
          }
          if (failures.length) options.reportCleanupFailure(new AggregateError(failures, 'RainyAgent cleanup failed.'))
        } finally {
          if (request) request.exit()
          else options.exit()
        }
      }).finally(() => { pendingQuit = undefined })
      return pendingQuit
    },
    switchTarget(prepare) {
      if (isClosing()) return Promise.resolve(closingResult)
      if (pendingSwitch !== undefined) return Promise.resolve({ ok: false, error: '执行环境正在切换，请等待应用重新打开。' })
      pendingSwitch = Promise.resolve().then(async () => {
        if (isClosing()) return closingResult
        const prepared = await prepare()
        if (!prepared.ok) return prepared
        let restarted = false
        try {
          if (isClosing()) return closingResult
          await prepared.commit()
          options.restart()
          restarted = true
          return { ok: true }
        } finally { if (!restarted) await prepared.resume() }
      }).finally(() => { pendingSwitch = undefined })
      return pendingSwitch
    },
  }
}
