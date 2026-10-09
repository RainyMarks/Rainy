/** Installer entry for the desktop carrier's adjacent offline tool archives. */
import { readFile } from 'node:fs/promises'
import { dirname, join, resolve } from 'node:path'
import { pathToFileURL } from 'node:url'
import { app, BrowserWindow, ipcMain } from 'electron'
import { installNativeToolPack, ToolPackInstallError } from './toolpack.ts'
import { assertToolPackPath, writeToolPackFile, writeToolPackRecord } from './toolpack-files.ts'
import { authenticateToolChannel } from './native-tools-update.ts'
import { parseReleaseKeyring } from './release-trust.ts'

/** Local progress; completedBytes applies only to the current named phase. */
export interface ToolPackWindowProgress {
  readonly phase: string
  readonly message: string
  readonly completedBytes: number
  readonly totalBytes: number
  readonly currentPath?: string
  readonly cancelling: boolean
}

/**
 * Install only the trusted tool pack described in this carrier's resources.
 * @param mediaDirectory - folder containing every adjacent unit archive.
 * @param silent - suppress the progress window for a silent installation.
 * @returns an installer process exit code after the result has been recorded.
 */
export async function runToolPackMaintenance(mediaDirectory: string, silent: boolean): Promise<number> {
  const installRoot = app.isPackaged ? dirname(app.getPath('exe')) : resolve(__dirname, '../toolpacks/maintenance-test')
  const stateRoot = join(installRoot, '.rainy-toolpack')
  const progress: { phase: string; message: string }[] = []
  const abort = new AbortController()
  let snapshot: ToolPackWindowProgress = { phase: 'preparing', message: '正在准备离线工具安装', completedBytes: 0, totalBytes: 0, cancelling: false }
  let window: BrowserWindow | undefined
  let finished = false
  const publish = (update: ToolPackWindowProgress): void => {
    snapshot = update
    if (window && !window.isDestroyed()) window.webContents.send('rainy:toolpack-progress', snapshot)
  }
  const cancel = (): void => {
    if (finished || abort.signal.aborted) return
    abort.abort()
    publish({ ...snapshot, cancelling: true, message: '正在安全停止，请稍候' })
  }
  const record = async (name: string, value: object): Promise<void> => {
    try { await writeToolPackRecord(join(stateRoot, name), value) }
    catch (error) { console.error('Installation report could not be saved', error instanceof Error ? error.message : String(error)) }
  }
  if (!silent) {
    const pageURL = pathToFileURL(join(__dirname, 'setup/toolpack.html')).href
    window = new BrowserWindow({ title: 'RainyAgent · 离线工具安装', width: 820, height: 590, minWidth: 500, minHeight: 430,
      show: false, webPreferences: { preload: join(__dirname, 'preload.cjs'),
        additionalArguments: [`--rainy-toolpack-page=${encodeURIComponent(pageURL)}`], nodeIntegration: false, contextIsolation: true, sandbox: true } })
    window.setMenu(null)
    const trusted = (event: Electron.IpcMainInvokeEvent): void => {
      if (!window || event.sender !== window.webContents || event.senderFrame !== window.webContents.mainFrame
        || event.senderFrame.url !== pageURL) throw new Error('安装请求来源无效')
    }
    ipcMain.handle('rainy:toolpack-status', (event) => { trusted(event); return snapshot })
    ipcMain.handle('rainy:toolpack-cancel', (event) => { trusted(event); cancel() })
    window.on('close', (event) => { if (!finished) { event.preventDefault(); cancel() } })
    window.webContents.setWindowOpenHandler(() => ({ action: 'deny' }))
    window.webContents.on('will-navigate', (event, url) => { if (url !== pageURL) event.preventDefault() })
    await window.loadURL(pageURL)
    window.show()
  }
  try {
    await assertToolPackPath(stateRoot)
    const resources = app.isPackaged ? process.resourcesPath : resolve(__dirname, '../resources')
    const channel = authenticateToolChannel(JSON.parse(await readFile(app.isPackaged ? join(resources, 'native-tools-channel.signed.json')
      : resolve(__dirname, '../toolpacks/native-tools-channel.v2.signed.json'), 'utf8')),
    parseReleaseKeyring(JSON.parse(await readFile(join(resources, 'native-tools-public-keys.json'), 'utf8'))))
    const metadataPath = join(stateRoot, `offline-metadata-${channel.metadata.id}.json`)
    await writeToolPackFile(metadataPath, JSON.stringify(channel.metadata))
    // Offline media carries every tool, so each unit is installed again from the adjacent archives.
    const result = await installNativeToolPack({ installRoot, mediaDirectory: resolve(mediaDirectory), metadataPath,
      replace: channel.metadata.units.map(unit => unit.path),
      onProgress: (update) => {
        if (progress.at(-1)?.phase !== update.phase) progress.push({ phase: update.phase, message: update.message })
        publish({ ...update, cancelling: abort.signal.aborted })
      }, signal: abort.signal,
    })
    await record('install-result.json', { ...result, progress })
    publish({ ...snapshot, phase: 'complete', message: '离线工具安装完成', cancelling: false })
    return 0
  } catch (error) {
    const code = error instanceof ToolPackInstallError ? error.code : 'install-failed'
    const message = error instanceof Error ? error.message : String(error)
    await record('install-error.log', { code, message })
    await record('install-result.json', { status: 'failed', code, message, progress })
    publish({ ...snapshot, phase: code === 'cancelled' ? 'cancelled' : 'error', message, cancelling: false })
    return code === 'cancelled' ? 3 : 1
  } finally {
    finished = true
    ipcMain.removeHandler('rainy:toolpack-status')
    ipcMain.removeHandler('rainy:toolpack-cancel')
    if (window && !window.isDestroyed()) window.destroy()
  }
}
