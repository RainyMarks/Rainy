/** Directory requests retain one action across native selection and workspace adoption. */
import { describe, expect, it, vi } from 'vitest'
import { IdeDirectorySelection } from '../src/client/ide-directory-selection.ts'

describe('native directory selection ownership', () => {
  it('shares overlapping requests through adoption and retains the first action', async () => {
    const choice = Promise.withResolvers<string | null>()
    const started = Promise.withResolvers<undefined>()
    const adopting = Promise.withResolvers<undefined>()
    const committed = Promise.withResolvers<undefined>()
    const choose = vi.fn(() => { started.resolve(undefined); return choice.promise })
    const adopt = vi.fn(() => { adopting.resolve(undefined); return committed.promise })
    const selection = new IdeDirectorySelection({ choose, adopt })
    const first = selection.open('attach')
    expect(selection.open('open')).toBe(first)
    expect(selection.open('attach')).toBe(first)
    expect(selection.pending.getSnapshot()).toBe(true)
    await started.promise
    expect(choose).toHaveBeenCalledOnce()
    choice.resolve('/shared')
    await adopting.promise
    expect(adopt).toHaveBeenCalledExactlyOnceWith('/shared', 'attach')
    expect(selection.open('open')).toBe(first)
    committed.resolve(undefined)
    await first
    expect(selection.pending.getSnapshot()).toBe(false)
    choose.mockResolvedValue(null)
    await selection.open('open')
    expect(choose).toHaveBeenCalledTimes(2)
    expect(adopt).toHaveBeenCalledOnce()
    selection.dispose()
  })

  it.each(['cancel', 'reject'] as const)('allows a new action after the chooser settles with %s', async (outcome) => {
    const choice = Promise.withResolvers<string | null>()
    const choose = vi.fn<() => Promise<string | null>>().mockReturnValueOnce(choice.promise).mockResolvedValue('/project')
    const adopt = vi.fn<(path: string, mode: 'open' | 'attach') => Promise<void>>().mockResolvedValue()
    const selection = new IdeDirectorySelection({ choose, adopt })
    const first = selection.open('attach')
    expect(selection.open('open')).toBe(first)
    const result = Promise.allSettled([first])
    if (outcome === 'cancel') choice.resolve(null)
    else choice.reject(new Error('Native picker failed'))
    expect((await result)[0]?.status).toBe(outcome === 'cancel' ? 'fulfilled' : 'rejected')
    expect(selection.pending.getSnapshot()).toBe(false)
    expect(adopt).not.toHaveBeenCalled()
    await selection.open('open')
    expect(choose).toHaveBeenCalledTimes(2)
    expect(adopt).toHaveBeenCalledExactlyOnceWith('/project', 'open')
    selection.dispose()
  })

  it('ignores a native response after the workspace owner unloads', async () => {
    const choice = Promise.withResolvers<string | null>()
    const started = Promise.withResolvers<undefined>()
    const choose = vi.fn(() => { started.resolve(undefined); return choice.promise })
    const adopt = vi.fn<(path: string, mode: 'open' | 'attach') => Promise<void>>().mockResolvedValue()
    const selection = new IdeDirectorySelection({ choose, adopt })
    const first = selection.open('open')
    await started.promise
    selection.dispose()
    choice.resolve('/late')
    await first
    await selection.open('attach')
    expect(choose).toHaveBeenCalledOnce()
    expect(adopt).not.toHaveBeenCalled()
  })
})
