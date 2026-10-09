/** Native directory choices preserve Windows spelling while using the selected WSL namespace. */
import { describe, expect, it, vi } from 'vitest'
import { chooseIdeDirectory, createIdeDirectoryPicker } from '../../src/main/ide-native.ts'
import type { IdeNativeDirectory } from '../../src/main/ide-native.ts'

describe('Windows IDE folder selection', () => {
  it.each(['chosen', 'cancelled', 'rejected'] as const)('keeps one dialog per owner until %s', async (outcome) => {
    const pending = Promise.withResolvers<IdeNativeDirectory | null>()
    const started = Promise.withResolvers<undefined>()
    const choose = vi.fn(() => { started.resolve(undefined); return pending.promise })
    const select = createIdeDirectoryPicker(choose)
    const first = select()
    expect(select()).toBe(first)
    expect(select()).toBe(first)
    await started.promise
    expect(choose).toHaveBeenCalledOnce()
    const settled = Promise.allSettled([first])
    if (outcome === 'rejected') pending.reject(new Error('Picker failed'))
    else pending.resolve(outcome === 'cancelled' ? null : { path: 'C:\\project', displayPath: 'C:\\project' })
    expect((await settled)[0]?.status).toBe(outcome === 'rejected' ? 'rejected' : 'fulfilled')
    choose.mockResolvedValue(null)
    await select()
    expect(choose).toHaveBeenCalledTimes(2)
  })

  it('keeps native chooser ownership separate for two windows', async () => {
    const first = Promise.withResolvers<IdeNativeDirectory | null>()
    const second = Promise.withResolvers<IdeNativeDirectory | null>()
    const firstChoose = vi.fn(() => first.promise)
    const secondChoose = vi.fn(() => second.promise)
    const one = createIdeDirectoryPicker(firstChoose)()
    const two = createIdeDirectoryPicker(secondChoose)()
    first.resolve(null)
    second.resolve(null)
    await Promise.all([one, two])
    expect(firstChoose).toHaveBeenCalledOnce()
    expect(secondChoose).toHaveBeenCalledOnce()
  })

  it('maps a selected Unicode directory with spaces without a shell command', async () => {
    const map = vi.fn(async () => '/mnt/c/项目/My Code\n')
    await expect(chooseIdeDirectory(async () => ({ canceled: false, filePaths: ['C:\\项目\\My Code'] }), map))
      .resolves.toEqual({ path: '/mnt/c/项目/My Code', displayPath: 'C:\\项目\\My Code' })
    expect(map).toHaveBeenCalledWith('C:\\项目\\My Code')
  })

  it('leaves cancellation without a mapping or workspace', async () => {
    const map = vi.fn(async () => '/unused')
    await expect(chooseIdeDirectory(async () => ({ canceled: true, filePaths: [] }), map)).resolves.toBeNull()
    expect(map).not.toHaveBeenCalled()
  })

  it('retains a mapping failure for the caller to display', async () => {
    await expect(chooseIdeDirectory(async () => ({ canceled: false, filePaths: ['C:\\code'] }), async () => { throw new Error('Distribution unavailable') }))
      .rejects.toThrow('Distribution unavailable')
  })

  it.each(['relative/path', 'C:\\bad\0name'])('rejects an invalid selection %s before mapping', async (path) => {
    const map = vi.fn(async () => '/unused')
    await expect(chooseIdeDirectory(async () => ({ canceled: false, filePaths: [path] }), map)).rejects.toThrow('Invalid selected')
    expect(map).not.toHaveBeenCalled()
  })

  it('rejects a response that is not one absolute WSL path', async () => {
    await expect(chooseIdeDirectory(async () => ({ canceled: false, filePaths: ['C:\\code'] }), async () => '/one\n/two'))
      .rejects.toThrow('could not be mapped')
  })
})
