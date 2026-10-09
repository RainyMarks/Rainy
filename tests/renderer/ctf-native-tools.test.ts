// @vitest-environment happy-dom
/** Native tool requests: coalescing, durable preference feedback and disposal. */
import { describe, expect, it, vi } from 'vitest'
import type { NativeToolCatalog, NativeToolLaunchResult, NativeToolsBridge } from '../../src/shared/native-tools-protocol.ts'
import { NativeToolsController } from '../../src/renderer/ctf/native-tools.ts'

const tool = { outdated: false, downloadBytes: 0 }
const catalog: NativeToolCatalog = { tools: [
  { ...tool, id: 'cyberchef', name: 'CyberChef', category: 'web', version: '10', launchKind: 'web', status: 'ready', verified: false, missing: [] },
  { ...tool, id: 'x64dbg', name: 'x64dbg', category: 'reverse', version: '2026', launchKind: 'desktop', status: 'ready', verified: true,
    missing: [], variants: [{ id: 'x32', name: 'x32dbg', status: 'ready' }] },
  { ...tool, id: '7zip', name: '7-Zip', category: 'misc', version: '25', launchKind: 'desktop', status: 'missing', verified: false, missing: ['7zFM.exe'] },
], preferences: { favorites: [], recent: ['x64dbg'] }, catalogOutdated: false, updateBytes: 0 }

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: Error) => void
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}

function fixture() {
  const bridge = { listTools: vi.fn<NativeToolsBridge['listTools']>().mockResolvedValue(catalog),
    checkToolUpdates: vi.fn<NativeToolsBridge['checkToolUpdates']>().mockResolvedValue({ phase: 'current', version: '1.0.0', error: '' }),
    getDownloadState: vi.fn<NativeToolsBridge['getDownloadState']>().mockResolvedValue({ phase: 'idle', completedBytes: 0, totalBytes: 10, error: '' }),
    installTools: vi.fn<NativeToolsBridge['installTools']>().mockResolvedValue(),
    removeTool: vi.fn<NativeToolsBridge['removeTool']>().mockResolvedValue(),
    repairTools: vi.fn<NativeToolsBridge['repairTools']>().mockResolvedValue(),
    cancelDownload: vi.fn<NativeToolsBridge['cancelDownload']>().mockResolvedValue(),
    onDownloadProgress: vi.fn<NativeToolsBridge['onDownloadProgress']>().mockReturnValue(vi.fn()),
    launchTool: vi.fn<NativeToolsBridge['launchTool']>().mockResolvedValue({ ok: true }),
    setFavorites: vi.fn<NativeToolsBridge['setFavorites']>().mockResolvedValue() }
  const toast = vi.fn()
  const copy = { opened: (name: string) => `Launch request sent for ${name}`, launchFailed: (name: string) => `Could not open ${name}`,
    favoritesFailed: () => 'Could not save favorites', favoriteSaved: (selected: boolean) => selected ? 'Added favorite' : 'Removed favorite',
    completed: (operation: string) => `Finished ${operation}`, downloadFailed: () => 'Download failed' }
  const controller = new NativeToolsController(bridge, copy, toast)
  return { controller, bridge, toast, copy }
}

describe('native tool directory', () => {
  it('coalesces operations, retains progress while hidden and refreshes availability after completion', async () => {
    const h = fixture()
    await h.controller.load()
    const pending = deferred<undefined>()
    h.bridge.installTools.mockReturnValueOnce(pending.promise)
    const running = h.controller.operate('install', ['cyberchef'])
    expect(h.controller.operate('remove', ['x64dbg'])).toBe(running)
    expect(h.controller.state.getSnapshot().download).toMatchObject({ phase: 'downloading', operation: 'install', tools: ['cyberchef'] })
    const receive = h.bridge.onDownloadProgress.mock.calls[0]![0]
    receive({ phase: 'downloading', completedBytes: 5, totalBytes: 10, error: '' })
    expect(h.controller.state.getSnapshot().download.completedBytes).toBe(5)
    await h.controller.cancelDownload()
    expect(h.bridge.cancelDownload).toHaveBeenCalledOnce()
    h.bridge.getDownloadState.mockResolvedValue({ phase: 'complete', completedBytes: 10, totalBytes: 10, error: '' })
    pending.resolve(undefined)
    await running
    expect(h.bridge.installTools).toHaveBeenCalledExactlyOnceWith(['cyberchef'])
    expect(h.bridge.removeTool).not.toHaveBeenCalled()
    expect(h.bridge.listTools).toHaveBeenCalledTimes(2)
    expect(h.toast).toHaveBeenCalledWith('Finished install', 'success')
  })

  it('routes updates, removals and repairs to their bridge operations', async () => {
    const h = fixture()
    h.bridge.getDownloadState.mockResolvedValue({ phase: 'complete', completedBytes: 0, totalBytes: 0, error: '' })
    await h.controller.operate('update')
    await h.controller.operate('remove', ['7zip'])
    await h.controller.operate('repair')
    expect(h.bridge.installTools).toHaveBeenCalledExactlyOnceWith([])
    expect(h.bridge.removeTool).toHaveBeenCalledExactlyOnceWith('7zip')
    expect(h.bridge.repairTools).toHaveBeenCalledOnce()
  })

  it('marks a checked channel as available while installed tools are outdated', async () => {
    const h = fixture()
    h.bridge.listTools.mockResolvedValue({ ...catalog, catalogOutdated: true })
    await h.controller.load()
    await h.controller.checkUpdates()
    h.bridge.listTools.mockResolvedValue(catalog)
    await h.controller.load()
    expect(h.controller.state.getSnapshot().update).toMatchObject({ phase: 'current', version: '1.0.0' })
    h.bridge.listTools.mockResolvedValue({ ...catalog, tools: catalog.tools.map(entry => ({ ...entry, outdated: entry.id === '7zip' })) })
    await h.controller.load()
    expect(h.controller.state.getSnapshot().update.phase).toBe('available')
  })

  it('retains download errors for retry and removes its progress listener on disposal', async () => {
    const h = fixture()
    await h.controller.load()
    h.bridge.installTools.mockRejectedValueOnce(new Error('offline'))
    await h.controller.operate('install', ['7zip'])
    expect(h.controller.state.getSnapshot().download).toMatchObject({ phase: 'error', error: 'offline' })
    h.controller.dispose()
    expect(h.bridge.onDownloadProgress.mock.results[0]!.value).toHaveBeenCalledOnce()
    const before = h.controller.state.getSnapshot()
    h.bridge.onDownloadProgress.mock.calls[0]![0]({ phase: 'complete', completedBytes: 10, totalBytes: 10, error: '' })
    expect(h.controller.state.getSnapshot()).toBe(before)
  })
  it('coalesces loads and keeps populated content when a refresh fails', async () => {
    const h = fixture()
    const pending = deferred<NativeToolCatalog>()
    h.bridge.listTools.mockReturnValueOnce(pending.promise)
    const first = h.controller.load()
    expect(h.controller.load()).toBe(first)
    pending.resolve(catalog)
    await first
    expect(h.bridge.listTools).toHaveBeenCalledOnce()
    h.bridge.listTools.mockRejectedValueOnce(new Error('directory unavailable'))
    await h.controller.load()
    expect(h.controller.state.getSnapshot()).toMatchObject({ phase: 'error', error: 'directory unavailable', tools: catalog.tools })
    await h.controller.load()
    expect(h.controller.state.getSnapshot().phase).toBe('ready')
  })

  it('coalesces launches, records successful recents and supports the x32 entry', async () => {
    const h = fixture()
    await h.controller.load()
    const pending = deferred<NativeToolLaunchResult>()
    h.bridge.launchTool.mockReturnValueOnce(pending.promise)
    const opening = h.controller.launch('cyberchef')
    await h.controller.launch('cyberchef')
    await h.controller.load()
    expect(h.bridge.listTools).toHaveBeenCalledOnce()
    expect(h.bridge.launchTool).toHaveBeenCalledOnce()
    expect(h.controller.state.getSnapshot().pending).toEqual(['cyberchef'])
    pending.resolve({ ok: true })
    await opening
    expect(h.controller.state.getSnapshot().preferences.recent).toEqual(['cyberchef', 'x64dbg'])
    expect(h.toast).toHaveBeenLastCalledWith('Launch request sent for CyberChef', 'success')
    await h.controller.launch('x64dbg', 'x32')
    expect(h.bridge.launchTool).toHaveBeenLastCalledWith('x64dbg', 'x32')
    expect(h.controller.state.getSnapshot().preferences.recent).toEqual(['x64dbg', 'cyberchef'])
    await h.controller.launch('7zip')
    expect(h.bridge.launchTool).toHaveBeenCalledTimes(2)
  })

  it('keeps launch failures out of recents and retains the catalog', async () => {
    const h = fixture()
    await h.controller.load()
    h.bridge.launchTool.mockResolvedValueOnce({ ok: false, error: 'File missing' })
    await h.controller.launch('cyberchef')
    expect(h.toast).toHaveBeenLastCalledWith('File missing', 'error')
    h.bridge.launchTool.mockRejectedValueOnce(new Error('Desktop disconnected'))
    await h.controller.launch('cyberchef')
    expect(h.toast).toHaveBeenLastCalledWith('Desktop disconnected', 'error')
    expect(h.controller.state.getSnapshot()).toMatchObject({ tools: catalog.tools, pending: [], preferences: catalog.preferences })
  })

  it('reports an accepted launch request with a preference warning', async () => {
    const h = fixture()
    await h.controller.load()
    h.bridge.launchTool.mockResolvedValueOnce({ ok: true, warning: 'Launch accepted, but recent history was not saved' })
    await h.controller.launch('cyberchef')
    expect(h.controller.state.getSnapshot().preferences.recent[0]).toBe('cyberchef')
    expect(h.toast).toHaveBeenLastCalledWith('Launch accepted, but recent history was not saved', 'warning')
    expect(h.controller.state.getSnapshot().pending).toEqual([])
  })

  it('commits favorites only after a serialized save and retains them on failure', async () => {
    const h = fixture()
    await h.controller.load()
    const pending = deferred<undefined>()
    h.bridge.setFavorites.mockReturnValueOnce(pending.promise)
    const saving = h.controller.toggleFavorite('cyberchef')
    await h.controller.toggleFavorite('x64dbg')
    expect(h.bridge.setFavorites).toHaveBeenCalledExactlyOnceWith(['cyberchef'])
    expect(h.controller.state.getSnapshot().preferences.favorites).toEqual([])
    pending.resolve(undefined)
    await saving
    expect(h.controller.state.getSnapshot().preferences.favorites).toEqual(['cyberchef'])
    h.bridge.setFavorites.mockRejectedValueOnce(new Error('Disk full'))
    await h.controller.toggleFavorite('cyberchef')
    expect(h.controller.state.getSnapshot().preferences.favorites).toEqual(['cyberchef'])
    expect(h.toast).toHaveBeenLastCalledWith('Disk full', 'error')
    await h.controller.toggleFavorite('cyberchef')
    expect(h.controller.state.getSnapshot().preferences.favorites).toEqual([])
    expect(h.toast).toHaveBeenLastCalledWith('Removed favorite', 'success')
  })

  it('does not publish load, launch or favorite completions after disposal', async () => {
    for (const operation of ['load', 'launch', 'favorite'] as const) {
      const h = fixture()
      await h.controller.load()
      const pending = deferred<undefined>()
      let work: Promise<void>
      if (operation === 'load') {
        h.bridge.listTools.mockImplementationOnce(async () => { await pending.promise; return catalog })
        work = h.controller.load()
      } else if (operation === 'launch') {
        h.bridge.launchTool.mockImplementationOnce(async () => { await pending.promise; return { ok: true } })
        work = h.controller.launch('cyberchef')
      } else {
        h.bridge.setFavorites.mockReturnValueOnce(pending.promise)
        work = h.controller.toggleFavorite('cyberchef')
      }
      h.controller.dispose()
      const frozen = h.controller.state.getSnapshot()
      pending.resolve(undefined)
      await work
      expect(h.controller.state.getSnapshot()).toBe(frozen)
      expect(h.toast).not.toHaveBeenCalled()
    }
  })

  it('exposes a desktop availability state without a preload bridge', async () => {
    const h = fixture()
    const controller = new NativeToolsController(undefined, h.copy, h.toast)
    await controller.load()
    await controller.launch('cyberchef')
    await controller.toggleFavorite('cyberchef')
    expect(controller.state.getSnapshot().phase).toBe('desktop-only')
    expect(h.toast).not.toHaveBeenCalled()
  })
})
