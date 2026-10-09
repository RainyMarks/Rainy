/** Show offline environment recovery before a WSL Host is available. */
import { BrowserWindow, dialog, ipcMain } from 'electron'
import { existsSync } from 'node:fs'
import { readFile, mkdir } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { z } from 'zod'
import { createEnvironmentSetup, writeEnvironmentRecord } from './environment.ts'
import type { EnvironmentAction, EnvironmentDistribution, EnvironmentSnapshot } from './environment.ts'
import { environmentMedia } from './environment-media.ts'

const actionSchema = z.union([
  z.object({ type: z.enum(['retry', 'resume', 'install-system-components', 'create-managed-distro']) }).strict(),
  z.object({ type: z.literal('select-existing'), distroName: z.string().min(1).max(256) }).strict(),
])

/** An inspected Linux environment, with the setup window retained during Host startup. */
export interface ReadyDesktopEnvironment {
  readonly distro: string
  readonly distributions: readonly EnvironmentDistribution[]
  /** Close the setup window and remove its isolated IPC handlers. */
  closeSetup(): void
}

/**
 * Open local diagnostics before querying or installing WSL.
 * @param options - carrier files, user settings, and installation media paths.
 * @returns a checked distribution after inspection or user-directed recovery.
 */
export async function prepareDesktopEnvironment(options: {
  readonly installRoot: string
  readonly userData: string
  readonly settingsPath: string
  readonly preloadPath: string
  readonly pagePath: string
  readonly mediaRoot: string
  readonly icon: string
  /** Main window that stays behind the setup window while startup waits for it. */
  readonly parent?: BrowserWindow
}): Promise<ReadyDesktopEnvironment> {
  const pageURL = pathToFileURL(options.pagePath).href
  const window = new BrowserWindow({ title: 'RainyAgent', width: 900, height: 670, minWidth: 680, minHeight: 520,
    show: false, icon: options.icon, backgroundColor: '#16191e', ...options.parent === undefined ? {} : { parent: options.parent },
    webPreferences: { preload: options.preloadPath, additionalArguments: [`--rainy-setup-page=${encodeURIComponent(pageURL)}`],
      nodeIntegration: false, contextIsolation: true, sandbox: true } })
  window.setMenu(null)
  const ready = Promise.withResolvers<{ distro: string; distributions: readonly EnvironmentDistribution[] }>()
  let settled = false
  let busy = false
  let pendingActions = 0
  let latest: EnvironmentSnapshot | undefined
  const trusted = (event: Electron.IpcMainInvokeEvent): boolean => event.sender === window.webContents
    && event.senderFrame === window.webContents.mainFrame && event.senderFrame.url === pageURL
  const update = (snapshot: EnvironmentSnapshot): void => {
    latest = snapshot
    busy = pendingActions > 0 || snapshot.busy
    if (!window.isDestroyed()) { window.setClosable(!busy); window.webContents.send('rainy:environment-progress', { ...snapshot, busy }) }
    if (snapshot.status === 'ready' && snapshot.distro !== undefined && !settled && pendingActions === 0) {
      settled = true
      ready.resolve({ distro: snapshot.distro, distributions: snapshot.distributions })
    }
  }
  const setup = createEnvironmentSetup({ installRoot: options.installRoot, userData: options.userData, mediaRoot: options.mediaRoot,
    async resolveMediaRoot() {
      const directories = [options.mediaRoot, join(options.installRoot, 'offline', 'environment'), join(dirname(options.installRoot), 'environment')]
      const available = directories.find(directory => Object.values(environmentMedia).every(item => existsSync(join(directory, item.file))))
      if (available) return available
      const selected = await dialog.showOpenDialog(window, { title: '选择完整离线包的 environment 文件夹', properties: ['openDirectory'] })
      const directory = selected.filePaths[0]
      if (selected.canceled || !directory) throw new Error('未选择 WSL 离线安装文件，现有环境保持不变。')
      return directory
    },
    async readDesktopSettings() {
      try { return z.record(z.string(), z.unknown()).parse(JSON.parse(await readFile(options.settingsPath, 'utf8'))) }
      catch (error) {
        if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return {}
        throw error
      }
    },
    async writeDesktopSettings(settings) {
      await mkdir(dirname(options.settingsPath), { recursive: true })
      await writeEnvironmentRecord(options.settingsPath, settings)
    }, onProgress: update,
  })
  ipcMain.handle('rainy:environment-inspect', async (event) => {
    if (!trusted(event)) throw new Error('环境请求来源无效')
    const snapshot = await setup.inspect()
    update(snapshot)
    return snapshot
  })
  ipcMain.handle('rainy:environment-act', async (event, value: unknown) => {
    if (!trusted(event)) throw new Error('环境请求来源无效')
    const action: EnvironmentAction = actionSchema.parse(value)
    pendingActions++
    busy = true
    window.setClosable(false)
    try {
      const snapshot = await setup.act(action)
      update(snapshot)
      return snapshot
    } finally {
      pendingActions--
      if (latest) update(latest)
    }
  })
  const cleanup = (): void => {
    ipcMain.removeHandler('rainy:environment-inspect')
    ipcMain.removeHandler('rainy:environment-act')
  }
  window.on('close', (event) => { if (busy) event.preventDefault() })
  window.on('closed', () => {
    cleanup()
    if (!settled) { settled = true; ready.reject(new Error('环境准备已取消')) }
  })
  window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
  window.webContents.on('will-navigate', (event, url) => { if (url !== pageURL) event.preventDefault() })
  window.webContents.session.setPermissionRequestHandler((_contents, _permission, callback) => { callback(false) })
  await window.loadURL(pageURL)
  if (!window.isDestroyed()) window.show()
  const result = await ready.promise
  return { ...result, closeSetup: () => { cleanup(); if (!window.isDestroyed()) window.destroy() } }
}
