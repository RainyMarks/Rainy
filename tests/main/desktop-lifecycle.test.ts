/** Promise barriers place exit and target selection at their owned transition points. */
import { describe, expect, it, vi } from 'vitest'
import { createDesktopLifecycle } from '../../src/main/desktop-lifecycle.ts'
import type { DesktopLifecycleOptions, PreparedTargetSwitch } from '../../src/main/desktop-lifecycle.ts'
import type { DraftFlushResult } from '../../src/main/saved-reload.ts'

function fixture(overrides: Partial<DesktopLifecycleOptions> = {}) {
  const options = {
    flush: vi.fn(async (): Promise<DraftFlushResult> => ({ ok: true })),
    retrySave: vi.fn(async (_error: string | undefined) => false),
    cleanup: [vi.fn(async () => {})],
    reportCleanupFailure: vi.fn((_error: AggregateError) => {}),
    exit: vi.fn(),
    restart: vi.fn(),
    ...overrides,
  }
  return { options, lifecycle: createDesktopLifecycle(options) }
}

function prepared(commit = vi.fn(async () => {})) {
  return { ok: true as const, commit, resume: vi.fn(async () => {}) }
}

describe('desktop exit ownership', () => {
  it('reserves exit during admission and leaves drafts and cleanup untouched when admission declines', async () => {
    const admitted = Promise.withResolvers<boolean>()
    const entered = Promise.withResolvers<undefined>()
    const request = { prepare: vi.fn(async () => { entered.resolve(undefined); return admitted.promise }), exit: vi.fn() }
    const { lifecycle, options } = fixture()
    const quitting = lifecycle.quit(false, request)
    await entered.promise
    expect(lifecycle.closing).toBe(true)
    expect(lifecycle.quitting).toBe(false)
    const inspection = vi.fn(async () => prepared())
    expect((await lifecycle.switchTarget(inspection)).ok).toBe(false)
    expect(inspection).not.toHaveBeenCalled()
    admitted.resolve(false)
    await quitting
    expect(options.flush).not.toHaveBeenCalled()
    expect(options.cleanup[0]).not.toHaveBeenCalled()
    expect(request.exit).not.toHaveBeenCalled()
    expect(lifecycle.closing).toBe(false)
  })

  it('releases admission after a preparation rejection and permits ordinary exit', async () => {
    const request = { prepare: vi.fn(async () => { throw new Error('Host unavailable') }), exit: vi.fn() }
    const { lifecycle, options } = fixture()
    await expect(lifecycle.quit(false, request)).rejects.toThrow('Host unavailable')
    expect(lifecycle.closing).toBe(false)
    expect(lifecycle.quitting).toBe(false)
    expect(options.flush).not.toHaveBeenCalled()
    expect(request.exit).not.toHaveBeenCalled()
    await lifecycle.quit()
    expect(options.exit).toHaveBeenCalledTimes(1)
  })

  it('keeps a prepared update from exiting when the user cancels saving', async () => {
    const request = { prepare: vi.fn(async () => true), exit: vi.fn() }
    const { lifecycle, options } = fixture({ flush: vi.fn(async () => ({ ok: false, error: 'storage unavailable' })) })
    await lifecycle.quit(false, request)
    expect(request.prepare).toHaveBeenCalledTimes(1)
    expect(options.cleanup[0]).not.toHaveBeenCalled()
    expect(request.exit).not.toHaveBeenCalled()
    expect(options.exit).not.toHaveBeenCalled()
    expect(lifecycle.closing).toBe(false)
    expect(lifecycle.quitting).toBe(false)
  })

  it('invokes the admitted custom exit only after saved drafts and every cleanup step', async () => {
    const saved = Promise.withResolvers<DraftFlushResult>()
    const cleaned = Promise.withResolvers<undefined>()
    const cleanupEntered = Promise.withResolvers<undefined>()
    const request = { prepare: vi.fn(async () => true), exit: vi.fn() }
    const { lifecycle, options } = fixture({
      flush: () => saved.promise,
      cleanup: [async () => { cleanupEntered.resolve(undefined); await cleaned.promise }],
    })
    const quitting = lifecycle.quit(false, request)
    expect(lifecycle.quit()).toBe(quitting)
    expect(request.exit).not.toHaveBeenCalled()
    saved.resolve({ ok: true })
    await cleanupEntered.promise
    expect(request.exit).not.toHaveBeenCalled()
    cleaned.resolve(undefined)
    await quitting
    expect(request.exit).toHaveBeenCalledTimes(1)
    expect(options.exit).not.toHaveBeenCalled()
  })

  it('does not let a later update request replace an ordinary exit already saving drafts', async () => {
    const saved = Promise.withResolvers<DraftFlushResult>()
    const saving = Promise.withResolvers<undefined>()
    const request = { prepare: vi.fn(async () => true), exit: vi.fn() }
    const { lifecycle, options } = fixture({ flush: async () => { saving.resolve(undefined); return saved.promise } })
    const quitting = lifecycle.quit()
    await saving.promise
    expect(lifecycle.quit(false, request)).toBe(quitting)
    saved.resolve({ ok: true })
    await quitting
    expect(request.prepare).not.toHaveBeenCalled()
    expect(request.exit).not.toHaveBeenCalled()
    expect(options.exit).toHaveBeenCalledTimes(1)
  })

  it('waits for a failing component operation and every later cleanup before exiting', async () => {
    const component = Promise.withResolvers<undefined>()
    const componentEntered = Promise.withResolvers<undefined>()
    const hostStopped = Promise.withResolvers<undefined>()
    const hostEntered = Promise.withResolvers<undefined>()
    const events: string[] = []
    const installError = new Error('component preparation failed')
    const windowError = new Error('native window cleanup failed')
    const { lifecycle, options } = fixture({ cleanup: [
      async () => { events.push('component'); componentEntered.resolve(undefined); await component.promise },
      async () => { events.push('ide') },
      async () => { events.push('native'); throw windowError },
      async () => { events.push('host'); hostEntered.resolve(undefined); await hostStopped.promise },
      async () => { events.push('lock') },
    ] })
    const quitting = lifecycle.quit()
    expect(lifecycle.closing).toBe(true)
    expect(lifecycle.quit()).toBe(quitting)
    await componentEntered.promise
    expect(events).toEqual(['component'])
    expect(options.exit).not.toHaveBeenCalled()
    component.reject(installError)
    await hostEntered.promise
    expect(events).toEqual(['component', 'ide', 'native', 'host'])
    expect(options.exit).not.toHaveBeenCalled()
    hostStopped.resolve(undefined)
    await quitting
    expect(events).toEqual(['component', 'ide', 'native', 'host', 'lock'])
    expect(options.reportCleanupFailure).toHaveBeenCalledExactlyOnceWith(expect.objectContaining({ errors: [installError, windowError] }))
    expect(options.exit).toHaveBeenCalledTimes(1)
    await lifecycle.quit()
    expect(options.exit).toHaveBeenCalledTimes(1)
  })

  it('keeps the Host and windows after cancelling a failed draft save and permits retry', async () => {
    const { lifecycle, options } = fixture({
      flush: vi.fn<() => Promise<DraftFlushResult>>()
        .mockResolvedValueOnce({ ok: false, error: 'storage unavailable' }).mockResolvedValueOnce({ ok: true }),
    })
    await lifecycle.quit()
    expect(options.retrySave).toHaveBeenCalledExactlyOnceWith('storage unavailable')
    expect(options.cleanup[0]).not.toHaveBeenCalled()
    expect(options.exit).not.toHaveBeenCalled()
    expect(lifecycle.closing).toBe(false)
    expect(lifecycle.quitting).toBe(false)
    await lifecycle.quit()
    expect(options.cleanup[0]).toHaveBeenCalledTimes(1)
    expect(options.exit).toHaveBeenCalledTimes(1)
  })

  it('retries saving before any cleanup and skips saving only for an acknowledged switch', async () => {
    const { lifecycle, options } = fixture({
      flush: vi.fn<() => Promise<DraftFlushResult>>()
        .mockResolvedValueOnce({ ok: false }).mockResolvedValueOnce({ ok: true }),
      retrySave: vi.fn(async () => true),
    })
    await lifecycle.quit()
    expect(options.flush).toHaveBeenCalledTimes(2)
    expect(options.exit).toHaveBeenCalledTimes(1)
    const saved = fixture()
    await saved.lifecycle.quit(true)
    expect(saved.options.flush).not.toHaveBeenCalled()
    expect(saved.options.exit).toHaveBeenCalledTimes(1)
  })

  it('releases exit admission when the draft-save request rejects', async () => {
    const failure = new Error('renderer unavailable')
    const { lifecycle, options } = fixture({ flush: vi.fn(async () => { throw failure }) })
    await expect(lifecycle.quit()).rejects.toBe(failure)
    expect(lifecycle.closing).toBe(false)
    expect(options.cleanup[0]).not.toHaveBeenCalled()
    expect(options.exit).not.toHaveBeenCalled()
  })
})

describe('desktop target-switch ownership', () => {
  it('allows a committed switch to request saved exit without waiting on itself', async () => {
    let exiting: Promise<void> | undefined
    const { lifecycle, options } = fixture({ restart: () => { exiting = lifecycle.quit(true) } })
    expect(await lifecycle.switchTarget(async () => prepared())).toEqual({ ok: true })
    expect(exiting).toBeDefined()
    await exiting
    expect(options.flush).not.toHaveBeenCalled()
    expect(options.cleanup[0]).toHaveBeenCalledTimes(1)
    expect(options.exit).toHaveBeenCalledTimes(1)
  })

  it('admits only one native-menu or IPC request while target inspection is pending', async () => {
    const inspected = Promise.withResolvers<PreparedTargetSwitch>()
    const entered = Promise.withResolvers<undefined>()
    const first = prepared()
    const { lifecycle, options } = fixture()
    const nativeMenu = lifecycle.switchTarget(async () => { entered.resolve(undefined); return inspected.promise })
    expect(lifecycle.switching).toBe(true)
    await entered.promise
    const ipcPrepare = vi.fn(async () => prepared())
    expect((await lifecycle.switchTarget(ipcPrepare)).ok).toBe(false)
    expect(ipcPrepare).not.toHaveBeenCalled()
    inspected.resolve(first)
    expect(await nativeMenu).toEqual({ ok: true })
    expect(first.commit).toHaveBeenCalledTimes(1)
    expect(first.resume).not.toHaveBeenCalled()
    expect(options.restart).toHaveBeenCalledTimes(1)
    expect(lifecycle.switching).toBe(false)
  })

  it('resumes the frozen Host and permits another target after a failed preference write', async () => {
    const failure = new Error('preferences unavailable')
    const first = prepared(vi.fn(async () => { throw failure }))
    const { lifecycle, options } = fixture()
    await expect(lifecycle.switchTarget(async () => first)).rejects.toBe(failure)
    expect(first.resume).toHaveBeenCalledTimes(1)
    expect(options.restart).not.toHaveBeenCalled()
    const second = prepared()
    expect(await lifecycle.switchTarget(async () => second)).toEqual({ ok: true })
    expect(second.commit).toHaveBeenCalledTimes(1)
    expect(options.restart).toHaveBeenCalledTimes(1)
  })

  it('releases target admission after preparation fails or returns active-work refusal', async () => {
    const { lifecycle, options } = fixture()
    await expect(lifecycle.switchTarget(async () => { throw new Error('project unavailable') })).rejects.toThrow('project unavailable')
    expect(await lifecycle.switchTarget(async () => ({ ok: false, error: 'Host busy' }))).toEqual({ ok: false, error: 'Host busy' })
    expect(lifecycle.switching).toBe(false)
    expect(options.restart).not.toHaveBeenCalled()
    expect(await lifecycle.switchTarget(async () => prepared())).toEqual({ ok: true })
  })

  it('cancels an uncommitted switch during exit and waits for the Host resume acknowledgement', async () => {
    const inspection = Promise.withResolvers<PreparedTargetSwitch>()
    const entered = Promise.withResolvers<undefined>()
    const resumed = Promise.withResolvers<undefined>()
    const resumeEntered = Promise.withResolvers<undefined>()
    const first = { ...prepared(), resume: vi.fn(async () => { resumeEntered.resolve(undefined); await resumed.promise }) }
    const { lifecycle, options } = fixture()
    const request = { prepare: vi.fn(async () => true), exit: vi.fn() }
    const switching = lifecycle.switchTarget(async () => { entered.resolve(undefined); return inspection.promise })
    await entered.promise
    const quitting = lifecycle.quit(false, request)
    expect(options.flush).not.toHaveBeenCalled()
    inspection.resolve(first)
    await resumeEntered.promise
    expect(first.commit).not.toHaveBeenCalled()
    expect(options.exit).not.toHaveBeenCalled()
    expect(request.prepare).not.toHaveBeenCalled()
    resumed.resolve(undefined)
    expect((await switching).ok).toBe(false)
    await quitting
    expect(first.resume).toHaveBeenCalledTimes(1)
    expect(options.restart).not.toHaveBeenCalled()
    expect(request.prepare).toHaveBeenCalledTimes(1)
    expect(request.exit).toHaveBeenCalledTimes(1)
    expect(options.exit).not.toHaveBeenCalled()
  })

  it('waits for an already admitted target commit before completing concurrent exit', async () => {
    const committed = Promise.withResolvers<undefined>()
    const entered = Promise.withResolvers<undefined>()
    const first = prepared(vi.fn(async () => { entered.resolve(undefined); await committed.promise }))
    const { lifecycle, options } = fixture()
    const switching = lifecycle.switchTarget(async () => first)
    await entered.promise
    const quitting = lifecycle.quit()
    expect(options.flush).not.toHaveBeenCalled()
    expect(options.exit).not.toHaveBeenCalled()
    committed.resolve(undefined)
    expect(await switching).toEqual({ ok: true })
    await quitting
    expect(first.resume).not.toHaveBeenCalled()
    expect(options.restart).toHaveBeenCalledTimes(1)
    expect(options.exit).toHaveBeenCalledTimes(1)
  })

  it('refuses new preparation once exit has been requested and drains a rejected switch', async () => {
    const inspection = Promise.withResolvers<PreparedTargetSwitch>()
    const entered = Promise.withResolvers<undefined>()
    const { lifecycle, options } = fixture()
    const switching = lifecycle.switchTarget(async () => { entered.resolve(undefined); return inspection.promise })
    const failure = expect(switching).rejects.toThrow('project unavailable')
    await entered.promise
    const quitting = lifecycle.quit()
    const rejected = vi.fn(async () => prepared())
    expect((await lifecycle.switchTarget(rejected)).ok).toBe(false)
    expect(rejected).not.toHaveBeenCalled()
    inspection.reject(new Error('project unavailable'))
    await failure
    await quitting
    expect(options.exit).toHaveBeenCalledTimes(1)
  })
})
