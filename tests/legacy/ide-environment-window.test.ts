/** Local-window source checks and install draining without opening a real desktop window. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { BrowserWindow } from 'electron'
import { createIdeEnvironmentWindow } from '../src/ide-environment-window.ts'
import type { IdeEnvironmentWindow } from '../src/ide-environment-window.ts'
import type { IdeEnvironmentInspection, IdeEnvironmentPlatform } from '../src/ide-environment.ts'

const control = vi.hoisted(() => ({
  windows: [] as object[],
  handlers: new Map<string, (event: Electron.IpcMainInvokeEvent, ...args: unknown[]) => unknown>(),
}))

vi.mock('electron', async () => {
  const { EventEmitter: Emitter } = await import('node:events')
  class Window extends Emitter {
    private destroyed = false
    readonly webContents = Object.assign(new Emitter(), { mainFrame: { url: '' }, send: vi.fn(), setWindowOpenHandler: vi.fn() })
    constructor() { super(); control.windows.push(this) }
    setMenu() {}
    setClosable() {}
    show() {}
    focus() {}
    async loadURL(url: string) { this.webContents.mainFrame.url = url }
    isDestroyed() { return this.destroyed }
    close() {
      let prevented = false
      this.emit('close', { preventDefault: () => { prevented = true } })
      if (!prevented) this.destroy()
    }
    destroy() { if (!this.destroyed) { this.destroyed = true; this.emit('closed') } }
  }
  return { BrowserWindow: Window, ipcMain: {
    handle: (name: string, handler: (event: Electron.IpcMainInvokeEvent, ...args: unknown[]) => unknown) => {
      control.handlers.set(name, handler)
    },
    removeHandler: (name: string) => { control.handlers.delete(name) },
  } }
})

let owner: IdeEnvironmentWindow | undefined
const releases: Array<() => void> = []
beforeEach(() => { control.windows.length = 0; control.handlers.clear() })
afterEach(async () => {
  for (const release of releases.splice(0)) release()
  await owner?.close()
  owner = undefined
  vi.restoreAllMocks()
})

function fixture() {
  let observed: IdeEnvironmentInspection = { os: 'ubuntu', osVersion: '26.04', architecture: 'amd64', mediaPresent: true, mediaMatches: true,
    packageCount: 92, incompletePackages: ['gdb'], tools: [{ name: 'gdb', path: null, ready: false }] }
  const ready = (): void => { observed = { ...observed, incompletePackages: [], tools: [{ name: 'gdb', path: '/usr/bin/gdb', ready: true }] } }
  const platform: IdeEnvironmentPlatform = { inspect: vi.fn(async () => observed), install: vi.fn(async () => { ready() }) }
  const parent = new BrowserWindow({})
  owner = createIdeEnvironmentWindow({ parent, distro: 'Selected', resourceRoot: '/resources/ide', preloadPath: '/local/preload.cjs',
    pagePath: '/local/setup/ide.html', platform })
  return { platform, ready, parent, window: () => {
    const window = control.windows.at(1)
    if (!(window instanceof BrowserWindow)) throw new Error('Missing setup window.')
    return window
  } }
}

function invoke(window: BrowserWindow, channel: string, value?: unknown): Promise<unknown> {
  const handler = control.handlers.get(channel)
  if (handler === undefined) throw new Error('Missing IPC handler.')
  const event = { sender: window.webContents, senderFrame: window.webContents.mainFrame } as Electron.IpcMainInvokeEvent
  return Promise.resolve().then(() => handler(event, value))
}

it('opens only on request, reuses one window, and rejects IPC from another window', async () => {
  const fixtureValue = fixture()
  expect(control.windows).toHaveLength(1)
  expect(fixtureValue.platform.inspect).not.toHaveBeenCalled()
  await owner?.open()
  await owner?.open()
  expect(control.windows).toHaveLength(2)
  expect(fixtureValue.platform.install).not.toHaveBeenCalled()
  await expect(invoke(fixtureValue.parent, 'rainy:ide-environment-inspect')).rejects.toThrow('来源无效')
  await expect(invoke(fixtureValue.window(), 'rainy:ide-environment-inspect')).resolves.toMatchObject({ status: 'needs-install' })
  await expect(invoke(fixtureValue.window(), 'rainy:ide-environment-act', { type: 'install', distro: 'other' })).rejects.toThrow()
  expect(fixtureValue.platform.install).not.toHaveBeenCalled()
  await owner?.close()
  expect(control.handlers.size).toBe(0)
})

it('keeps setup owned after a forced close and drains installation before carrier disposal resolves', async () => {
  const fixtureValue = fixture()
  const entered = Promise.withResolvers<undefined>()
  const release = Promise.withResolvers<undefined>()
  releases.push(() => { release.resolve(undefined) })
  vi.mocked(fixtureValue.platform.install).mockImplementation(async () => {
    entered.resolve(undefined)
    await release.promise
    fixtureValue.ready()
  })
  await owner?.open()
  const current = fixtureValue.window()
  const installing = invoke(current, 'rainy:ide-environment-act', { type: 'install' })
  await entered.promise
  current.close()
  expect(current.isDestroyed()).toBe(false)
  current.destroy()
  expect(control.handlers.size).toBe(0)
  let disposed = false
  const closing = owner?.close().then(() => { disposed = true })
  expect(disposed).toBe(false)
  release.resolve(undefined)
  await installing
  await closing
  expect(disposed).toBe(true)
  expect(fixtureValue.platform.install).toHaveBeenCalledOnce()
})
