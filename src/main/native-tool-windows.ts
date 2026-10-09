/** Native tool windows and the trusted desktop catalog bridge. */
import { BrowserWindow, ipcMain } from 'electron'
import { NativeToolsLibrary, parseNativeFavorites, parseNativeLaunch } from './native-tools.ts'
import type { NativeToolsRequest } from './native-tools-download.ts'
import { startNativeProcess } from './native-tool-process.ts'
import { serveNativeTool, type NativeToolWebPage } from './native-tool-web.ts'
import { join } from 'node:path'
import { NativeToolsDownloader } from './native-tools-download.ts'
import type { ReleaseKeyring } from './release-trust.ts'

/**
 * Attach human tool operations to one authenticated main window.
 * @param options - main window, authenticated Host origin, and installed tool paths.
 * @returns complete cleanup of IPC handlers and owned offline webpage servers.
 */
export function installNativeTools(options: {
  readonly window: BrowserWindow
  readonly origin: string
  readonly installRoot: string
  readonly userData: string
  readonly download?: {
    /** Signed per-tool channel shipped with this carrier. */
    readonly channelPath: string
    readonly keys: ReleaseKeyring
    /** The carrier holds the installation lock of installRoot. */
    readonly installRootLocked: boolean
  }
}): { close(): Promise<void>; maintain(): Promise<number> } {
  const webpages = new Map<BrowserWindow, NativeToolWebPage>()
  const closingPages = new Set<Promise<void>>()
  let closed = false
  let installing = false
  let installOperation: Promise<void> | undefined
  let cancelRequested = false
  const isCancelled = (): boolean => cancelRequested
  const onlineRoot = join(options.userData, 'native-tools')
  const download = options.download === undefined ? undefined : new NativeToolsDownloader({
    onlineRoot, cacheRoot: join(options.userData, 'native-tools-downloads'),
    legacyRoot: options.installRoot, legacyLocked: options.download.installRootLocked,
    channelPath: options.download.channelPath, keys: options.download.keys,
    fetch: (input, init) => options.window.webContents.session.fetch(input instanceof URL ? input.href : input, init),
    publish: (state) => { if (!closed && !options.window.isDestroyed()) options.window.webContents.send('rainy:tools-download-progress', state) },
  })
  const isClosed = (): boolean => closed
  const assertNoWebpages = (): void => { if (webpages.size > 0) throw new Error('请先关闭已打开的工具窗口，再下载或更新工具包') }
  const closePage = (page: NativeToolWebPage): Promise<void> => {
    const closing = page.close()
    closingPages.add(closing)
    void closing.catch((error: unknown) => { console.error('Offline tool resource cleanup failed', error) }).finally(() => { closingPages.delete(closing) })
    return closing
  }
  const library = new NativeToolsLibrary({ installRoot: options.installRoot, userData: options.userData,
    ...download === undefined ? {} : { inventory: () => download.inventory() },
    start: async (invocation) => {
      if (isClosed()) throw new Error('RainyAgent 正在关闭')
      if (installing) throw new Error('请等待工具安装完成后再打开工具')
      if (invocation.kind !== 'web') {
        await startNativeProcess(invocation, globalThis.process.env)
        return
      }
      const page = await serveNativeTool(invocation)
      if (isClosed()) { await page.close(); throw new Error('RainyAgent 正在关闭') }
      const window = new BrowserWindow({ title: invocation.name, width: 1240, height: 860, minWidth: 700, minHeight: 500,
        show: false, webPreferences: { nodeIntegration: false, contextIsolation: true, sandbox: true,
          partition: `rainy-tool-${invocation.id}`, spellcheck: false } })
      window.setMenu(null)
      webpages.set(window, page)
      window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
      window.webContents.on('will-navigate', (event, url) => { if (new URL(url).origin !== page.origin) event.preventDefault() })
      window.webContents.session.setPermissionRequestHandler((_contents, permission, callback) => { callback(permission === 'clipboard-sanitized-write') })
      window.on('closed', () => { webpages.delete(window); void closePage(page) })
      try { await window.loadURL(page.url); window.show() }
      catch (error) { if (!window.isDestroyed()) window.destroy(); await page.close(); throw error }
    },
  })
  const trusted = (event: Electron.IpcMainInvokeEvent): void => {
    if (closed || event.sender !== options.window.webContents || event.senderFrame !== options.window.webContents.mainFrame
      || new URL(event.senderFrame.url).origin !== options.origin) throw new Error('工具请求来源无效')
  }
  ipcMain.handle('rainy:tools-list', async (event) => { trusted(event); return library.listTools() })
  ipcMain.handle('rainy:tools-check-updates', async (event) => {
    trusted(event)
    if (!download) throw new Error('当前版本未提供在线工具包，请更新 RainyAgent')
    return download.checkUpdates()
  })
  ipcMain.handle('rainy:tools-download-state', (event) => {
    trusted(event)
    if (!download) throw new Error('当前版本未提供在线工具包，请更新 RainyAgent')
    return download.status()
  })
  const operate = (request: NativeToolsRequest): Promise<void> => {
    if (!download) throw new Error('当前版本未提供在线工具包，请更新 RainyAgent')
    if (installOperation) throw new Error('另一个工具操作正在进行，请等待完成或先取消')
    assertNoWebpages()
    installing = true
    cancelRequested = false
    installOperation = (async () => {
      await library.waitForIdle()
      // A webpage launch accepted before installation began can open its window while the library drains.
      assertNoWebpages()
      if (!isClosed() && !isCancelled()) await download.start(request)
    })().finally(() => { installing = false; installOperation = undefined })
    return installOperation
  }
  ipcMain.handle('rainy:tools-install', async (event, value: unknown) => {
    trusted(event)
    const tools = parseNativeFavorites(value)
    return operate(tools.length ? { operation: 'install', tools } : { operation: 'update' })
  })
  ipcMain.handle('rainy:tools-remove', async (event, value: unknown) => {
    trusted(event)
    return operate({ operation: 'remove', tools: [parseNativeLaunch({ id: value }).id] })
  })
  ipcMain.handle('rainy:tools-repair', async (event) => { trusted(event); return operate({ operation: 'repair' }) })
  ipcMain.handle('rainy:tools-download-cancel', async (event) => { trusted(event); cancelRequested = true; await download?.cancel(); await installOperation })
  ipcMain.handle('rainy:tools-launch', async (event, value: unknown) => { trusted(event); const request = parseNativeLaunch(value); return library.launchTool(request.id, request.variant) })
  ipcMain.handle('rainy:tools-favorites', async (event, value: unknown) => { trusted(event); await library.setFavorites(parseNativeFavorites(value)) })
  let closing: Promise<void> | undefined
  const close = (): Promise<void> => {
    if (closing) return closing
    closed = true
    ipcMain.removeHandler('rainy:tools-list')
    ipcMain.removeHandler('rainy:tools-check-updates')
    ipcMain.removeHandler('rainy:tools-launch')
    ipcMain.removeHandler('rainy:tools-favorites')
    ipcMain.removeHandler('rainy:tools-download-state')
    ipcMain.removeHandler('rainy:tools-install')
    ipcMain.removeHandler('rainy:tools-remove')
    ipcMain.removeHandler('rainy:tools-repair')
    ipcMain.removeHandler('rainy:tools-download-cancel')
    closing = Promise.resolve().then(async () => {
      const owned = [...webpages]
      webpages.clear()
      for (const [window] of owned) if (!window.isDestroyed()) window.destroy()
      await download?.close()
      await installOperation
      await library.waitForIdle()
      const results = await Promise.allSettled([...owned.map(([, page]) => page.close()), ...closingPages])
      const failures = results.filter(result => result.status === 'rejected')
      if (failures.length) throw new AggregateError(failures.map((result): unknown => result.reason), 'Offline tool resources did not finish closing')
    })
    return closing
  }
  const maintain = (): Promise<number> => closed || installOperation || !download ? Promise.resolve(0) : download.maintain()
  options.window.on('closed', () => { void close().catch((error: unknown) => { console.error('Native tool window cleanup failed', error) }) })
  return { close, maintain }
}
