/** A retained local development setup window, opened only by a human IDE action. */
import { BrowserWindow, ipcMain } from 'electron'
import { pathToFileURL } from 'node:url'
import { createIdeEnvironmentSetup, parseIdeEnvironmentAction, resolveIdeEnvironmentConfig } from './ide-environment.ts'
import type { IdeEnvironmentConfig, IdeEnvironmentPlatform, IdeEnvironmentSetup, IdeEnvironmentSnapshot } from './ide-environment.ts'
import { createWindowsIdeEnvironmentPlatform } from './ide-environment-platform.ts'

/** Fixed carrier inputs for a selected distribution; no target comes from the setup page. */
export interface IdeEnvironmentWindowOptions {
  readonly parent: BrowserWindow
  readonly distro: string
  readonly resourceRoot: string
  readonly preloadPath: string
  readonly pagePath: string
  readonly icon?: string
  readonly config?: IdeEnvironmentConfig
  readonly platform?: IdeEnvironmentPlatform
}

/** Window lifetime owned by the main carrier. */
export interface IdeEnvironmentWindow {
  /** @returns resolution after the local window is shown; installation still requires a separate click. */
  open(): Promise<void>
  /** @returns resolution after admitted setup work drains, handlers are removed, and the window is destroyed. */
  close(): Promise<void>
}

const INSPECT = 'rainy:ide-environment-inspect'
const ACT = 'rainy:ide-environment-act'
const CLOSE = 'rainy:ide-environment-close'
const PROGRESS = 'rainy:ide-environment-progress'

/**
 * Construct a window owner without creating a window or running WSL commands.
 * @param options - parent, selected distribution, trusted local page and packaged resources.
 * @returns an explicitly opened development wizard and quiescent shutdown.
 */
export function createIdeEnvironmentWindow(options: IdeEnvironmentWindowOptions): IdeEnvironmentWindow {
  const config = options.config ?? resolveIdeEnvironmentConfig()
  const pageURL = pathToFileURL(options.pagePath).href
  let window: BrowserWindow | undefined
  let setup: IdeEnvironmentSetup | undefined
  let opening: Promise<void> | undefined
  let closing = false
  let disposal: Promise<void> | undefined
  let pendingActions = 0
  const pendingDisposals = new Set<Promise<void>>()

  const cleanupHandlers = (): void => {
    ipcMain.removeHandler(INSPECT)
    ipcMain.removeHandler(ACT)
    ipcMain.removeHandler(CLOSE)
  }

  const openInternal = async (): Promise<void> => {
    const current = new BrowserWindow({ title: 'RainyAgent · 开发工具准备', parent: options.parent,
      width: 900, height: 740, minWidth: 680, minHeight: 560, show: false, backgroundColor: '#16191e',
      ...options.icon === undefined ? {} : { icon: options.icon },
      webPreferences: { preload: options.preloadPath, additionalArguments: [`--rainy-ide-environment-page=${encodeURIComponent(pageURL)}`],
        nodeIntegration: false, contextIsolation: true, sandbox: true } })
    window = current
    current.setMenu(null)
    const update = (snapshot: IdeEnvironmentSnapshot): void => {
      if (current.isDestroyed()) return
      const busy = pendingActions > 0 || snapshot.busy
      current.setClosable(!busy)
      current.webContents.send(PROGRESS, { ...snapshot, busy })
    }
    const controller = createIdeEnvironmentSetup({ distro: options.distro, config, onProgress: update,
      platform: options.platform ?? createWindowsIdeEnvironmentPlatform({
        distro: options.distro, resourceRoot: options.resourceRoot, config,
      }) })
    setup = controller
    const trusted = (event: Electron.IpcMainInvokeEvent): void => {
      if (current.isDestroyed() || event.sender !== current.webContents || event.senderFrame !== current.webContents.mainFrame
        || event.senderFrame.url !== pageURL) throw new Error('开发工具准备请求来源无效。')
    }
    ipcMain.handle(INSPECT, async (event) => { trusted(event); return controller.inspect() })
    ipcMain.handle(ACT, async (event, value: unknown) => {
      trusted(event)
      const action = parseIdeEnvironmentAction(value)
      pendingActions++
      current.setClosable(false)
      try { return await controller.act(action) }
      finally { pendingActions--; update(controller.snapshot()) }
    })
    ipcMain.handle(CLOSE, (event) => { trusted(event); current.close() })
    current.on('close', (event) => { if (pendingActions > 0 || controller.snapshot().busy) event.preventDefault() })
    current.on('closed', () => {
      if (window === current) {
        cleanupHandlers()
        window = undefined
        setup = undefined
      }
      const pending = controller.close()
      pendingDisposals.add(pending)
      void pending.finally(() => { pendingDisposals.delete(pending) })
    })
    current.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
    current.webContents.on('will-navigate', (event, url) => { if (url !== pageURL) event.preventDefault() })
    try {
      await current.loadURL(pageURL)
      if (!current.isDestroyed()) current.show()
    } catch (error) {
      if (!current.isDestroyed()) current.destroy()
      await controller.close()
      throw error
    }
  }

  return {
    open: () => {
      if (closing) return Promise.reject(new Error('开发工具向导正在关闭。'))
      if (opening !== undefined) return opening
      if (window !== undefined && !window.isDestroyed()) {
        window.show()
        window.focus()
        return Promise.resolve()
      }
      opening = openInternal().finally(() => { opening = undefined })
      return opening
    },
    close: () => {
      closing = true
      disposal ??= (async () => {
        await opening?.catch((_failedWindowLoad: unknown) => { /* Window-load cleanup is owned by openInternal. */ })
        await setup?.close()
        cleanupHandlers()
        if (window !== undefined && !window.isDestroyed()) window.destroy()
        await Promise.allSettled([...pendingDisposals])
      })()
      return disposal
    },
  }
}
