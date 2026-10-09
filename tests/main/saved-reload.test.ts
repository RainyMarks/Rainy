/** Native reload paths preserve drafts until the existing save bridge confirms completion. */
import { describe, expect, it, vi } from 'vitest'
import { createSavedReload, nativeReloadShortcut } from '../../src/main/saved-reload.ts'
import type { DraftFlushResult } from '../../src/main/saved-reload.ts'

describe('native reload shortcuts', () => {
  const input = { type: 'keyDown' as const, key: 'r', control: false, meta: false, shift: false, alt: false }

  it('recognizes F5 and platform reload keys with cache bypass modifiers', () => {
    expect(nativeReloadShortcut({ ...input, key: 'F5' })).toBe(false)
    expect(nativeReloadShortcut({ ...input, key: 'F5', control: true })).toBe(true)
    expect(nativeReloadShortcut({ ...input, control: true })).toBe(false)
    expect(nativeReloadShortcut({ ...input, key: 'R', meta: true, shift: true })).toBe(true)
    expect(nativeReloadShortcut(input)).toBeUndefined()
    expect(nativeReloadShortcut({ ...input, control: true, type: 'keyUp' })).toBeUndefined()
    expect(nativeReloadShortcut({ ...input, control: true, alt: true })).toBeUndefined()
  })
})

describe('saved native reload', () => {
  it('coalesces shortcuts and waits for the save acknowledgement before cache-bypassing reload', async () => {
    const saved = Promise.withResolvers<DraftFlushResult>()
    const flush = vi.fn(() => saved.promise)
    const reload = vi.fn()
    const request = createSavedReload({ flush, reload, isClosing: () => false, reportFailure: async () => {} })
    const first = request(true)
    expect(request()).toBe(first)
    expect(flush).toHaveBeenCalledTimes(1)
    expect(reload).not.toHaveBeenCalled()
    saved.resolve({ ok: true })
    await first
    expect(reload).toHaveBeenCalledExactlyOnceWith(true)
  })

  it('keeps the document after a failed save and permits a successful retry', async () => {
    const flush = vi.fn<() => Promise<DraftFlushResult>>()
      .mockResolvedValueOnce({ ok: false, error: 'Storage unavailable' }).mockResolvedValueOnce({ ok: true })
    const reload = vi.fn()
    const reportFailure = vi.fn(async (_error: string) => {})
    const request = createSavedReload({ flush, reload, reportFailure, isClosing: () => false })
    await request()
    expect(reload).not.toHaveBeenCalled()
    expect(reportFailure).toHaveBeenCalledExactlyOnceWith('Storage unavailable')
    await request()
    expect(reload).toHaveBeenCalledExactlyOnceWith(false)
  })

  it('does not reload when application exit begins during a pending save', async () => {
    const saved = Promise.withResolvers<DraftFlushResult>()
    let closing = false
    const reload = vi.fn()
    const reportFailure = vi.fn(async (_error: string) => {})
    const request = createSavedReload({ flush: () => saved.promise, reload, reportFailure, isClosing: () => closing })
    const pending = request()
    closing = true
    saved.resolve({ ok: true })
    await pending
    expect(reload).not.toHaveBeenCalled()
    expect(reportFailure).not.toHaveBeenCalled()
  })
})
