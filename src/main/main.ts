/** RainyAgent desktop carrier for one explicitly selected native Windows or WSL Host. */
import { app, BrowserWindow, dialog, Menu, screen, shell, ipcMain, nativeTheme } from 'electron'
import { mkdirSync, appendFileSync, existsSync, writeFileSync } from 'node:fs'
import { dirname, join, posix, resolve, win32 } from 'node:path'
import { execFile, execFileSync } from 'node:child_process'
import { promisify } from 'node:util'
import { WslHostTransport, WindowsHostTransport } from './transport.ts'
import type { HostOptions, HostTransport } from './transport.ts'
import { CAPTION_HEIGHT, captionColors } from './window-chrome.ts'
import { nativeMenuTemplate } from './native-menus.ts'
import { createSavedReload, nativeReloadShortcut } from './saved-reload.ts'
import type { DraftFlushResult } from './saved-reload.ts'
import { createDesktopLifecycle } from './desktop-lifecycle.ts'
import electronUpdater from 'electron-updater'
import { RainyUpdates } from './updates.ts'
import { updateMessages } from './update-messages.ts'
import { createStrataManager } from './strata.ts'
import { resolveBudget } from './budget.ts'
import { randomUUID } from 'node:crypto'
import { prepareDesktopEnvironment } from './native-environment-window.ts'
import { installNativeTools } from './native-tool-windows.ts'
import { runToolPackMaintenance } from './toolpack-maintenance.ts'
import { OptionalModules } from './optional-modules.ts'
import { removeRetiredResources } from './retired-resources.ts'
import { acquireToolPackLock } from './toolpack.ts'
import { chooseIdeDirectory, createIdeDirectoryPicker } from './ide-native.ts'
import { createIdeEnvironmentWindow } from './ide-environment-window.ts'
import { listExecutionTargets, readDesktopPreferences, savedExecutionTarget, saveExecutionTarget } from './execution-targets.ts'
import type { ExecutionTarget, PendingProjectTarget } from './execution-targets.ts'
import { createProjectRegistry } from './project-registry.ts'
import { environmentComponentSchema, installWindowsComponent, readEnvironmentComponentCatalog } from './environment-components.ts'
import { readFile, stat } from 'node:fs/promises'
import { embeddedReleaseKeys, parseReleaseKeyring } from './release-trust.ts'
import { ensureReleaseIntegrity, ReleaseIntegrityError } from './release-integrity.ts'
import type { ReleaseIntegrity } from './release-integrity.ts'
import { startupPage, startupProgress, WINDOW_BACKGROUND } from './startup-splash.ts'
import { initialPlacement, readWindowState, writeWindowState } from './window-state.ts'
import type { WindowState } from './window-state.ts'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import { allowsClipboardWrite } from './clipboard-policy.ts'

const run = promisify(execFile)
app.setName('RainyAgent')
app.setAppUserModelId('dev.rainy.agent')
const userDataDirectory = app.commandLine.getSwitchValue('user-data-dir')
app.setPath('userData', userDataDirectory ? resolve(userDataDirectory) : join(app.getPath('appData'), 'RainyAgent'))
let transport: HostTransport | undefined
let flushWorkbench: () => Promise<DraftFlushResult> = () => Promise.resolve({ ok: true })
let closeNativeTools: () => Promise<void> = () => Promise.resolve()
let closeIdeEnvironment: () => Promise<void> = () => Promise.resolve()
let closeStrata: () => Promise<void> = () => Promise.resolve()
let closeModules: () => Promise<void> = () => Promise.resolve()
let releaseToolPack: (() => Promise<void>) | undefined
let componentInstallation: Promise<void> | undefined
let updates: RainyUpdates | undefined
let cleanupFailed = false
// Setup windows may close before the workbench takes ownership of the application.
let mainWindowCreated = false
const lifecycle = createDesktopLifecycle({
  flush: () => flushWorkbench(),
  retrySave: async (error) => {
    const answer = await dialog.showMessageBox({ type: 'error', title: 'RainyAgent',
      message: '草稿尚未保存', detail: error ?? '保存失败，窗口和草稿已保留。',
      buttons: ['重试保存并退出', '取消退出'], defaultId: 0, cancelId: 1 })
    return answer.response === 0
  },
  cleanup: [async () => { await componentInstallation }, () => closeIdeEnvironment(), () => closeNativeTools(),
    async () => { await transport?.stop() }, () => closeStrata(), () => closeModules(), async () => { await releaseToolPack?.() }],
  reportCleanupFailure: (error) => { cleanupFailed = true; console.error('RainyAgent cleanup failed:', error) },
  exit: () => { app.quit() },
  restart: () => { app.relaunch(); quitAfterSave(true) },
})

function requireWslDistribution(target: ExecutionTarget): string {
  if (target.distro === undefined) throw new Error('The selected WSL execution target has no distribution')
  return target.distro
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/** Translate one path with the distribution's wslpath; arguments are never shell-interpolated. */
async function wslPath(distro: string, direction: '-u' | '-w', path: string): Promise<string> {
  return (await run('wsl.exe', ['-d', distro, '--exec', 'wslpath', direction, path], { windowsHide: true, timeout: 15000 })).stdout.trim()
}

/** Translate several Windows paths with one wsl.exe call, which also absorbs a cold VM start; one output line per path. */
async function wslPaths(distro: string, paths: readonly string[]): Promise<string[]> {
  const output = (await run('wsl.exe', ['-d', distro, '--exec', 'sh', '-c', 'for p in "$@"; do wslpath -u "$p"; done', 'sh', ...paths],
    { windowsHide: true, timeout: 120000 })).stdout
  const lines = output.split('\n').map(line => line.replace(/\r$/u, '')).filter(line => line !== '')
  if (lines.length !== paths.length || !lines.every(line => posix.isAbsolute(line))) throw new Error('WSL 路径转换结果无效。')
  return lines
}

/** @param logPath - carrier log for a missing launcher. @returns the Windows uvx executable used by the official IDA MCP server. */
async function findUvx(logPath: string): Promise<string | undefined> {
  try {
    const candidates = (await run('where.exe', ['uvx.exe'], { windowsHide: true, timeout: 5000 })).stdout
    return candidates.split(/\r?\n/).find(path => win32.isAbsolute(path) && existsSync(path))
  } catch (error) {
    appendFileSync(logPath, `IDA MCP launcher unavailable: ${error instanceof Error ? error.name : 'unknown error'}\n`)
    return undefined
  }
}

/** @param value - persisted preference. @param target - resolved target. @returns whether the preference already records it. */
function sameTarget(value: unknown, target: ExecutionTarget): boolean {
  return value !== null && typeof value === 'object' && 'id' in value && value.id === target.id && 'kind' in value && value.kind === target.kind
    && 'label' in value && value.label === target.label && ('distro' in value ? value.distro : undefined) === target.distro
}

/** Per-phase startup durations written to the carrier log. */
function startupTimeline(): { mark(phase: string): void; summary(): string } {
  const started = performance.now()
  let previous = started
  const phases: string[] = []
  return {
    mark(phase) { const now = performance.now(); phases.push(`${phase} ${Math.round(now - previous)} ms`); previous = now },
    summary() { return `${phases.join(', ')}; total ${Math.round(performance.now() - started)} ms` },
  }
}

/** A full resource re-verification is due at most once per day after the stamp was confirmed. */
const RELEASE_RECHECK_INTERVAL_MS = 24 * 60 * 60 * 1000
/** Background checks wait until the workbench has settled. */
const BACKGROUND_CHECK_DELAY_MS = 60_000

/** Bodies of private Host control responses are untrusted and may not be JSON. */
async function controlBody(response: Response): Promise<unknown> {
  try { return await response.json() }
  catch (_malformedBody) { return undefined } // Callers report a typed failure for any unusable body.
}

/** Save through the renderer while the WSL Host is still serving its durable state API. */
function quitAfterSave(draftsSaved = false): void {
  if (lifecycle.closing) return
  void lifecycle.quit(draftsSaved).catch(async (error: unknown) => {
    await dialog.showMessageBox({ type: 'error', message: '退出前保存失败', detail: errorText(error) })
  })
}

async function start(): Promise<void> {
  const timeline = startupTimeline()
  const userData = app.getPath('userData')
  mkdirSync(userData, { recursive: true })
  const logPath = join(userData, 'host.log')
  const settingsPath = join(userData, 'desktop.json')
  const installRoot = app.isPackaged ? dirname(app.getPath('exe')) : resolve(__dirname, '..')
  const resourceRoot = app.isPackaged ? process.resourcesPath : resolve(__dirname, '..', 'runtime')
  const [lock, preferences] = await Promise.all([acquireToolPackLock(installRoot), readDesktopPreferences(settingsPath)])
  releaseToolPack = lock
  let target = savedExecutionTarget(preferences)
  // A launch that already recorded its resolved WSL target skips the environment window and the registry lookup.
  const knownTarget = target.kind === 'wsl' && preferences.executionTarget !== undefined
  // Booting the WSL VM and locating the IDA launcher overlap with resource admission and the first paint.
  if (knownTarget) void run('wsl.exe', ['-d', requireWslDistribution(target), '--exec', 'true'], { windowsHide: true, timeout: 120000 })
    .catch((error: unknown) => { appendFileSync(logPath, `WSL warm-up failed: ${errorText(error)}\n`) })
  const uvxLookup = findUvx(logPath)
  const preloadPath = join(__dirname, 'preload.cjs')
  const icon = app.isPackaged ? join(resourceRoot, 'icon.ico') : resolve(__dirname, '../build/icon.ico')
  const dark = nativeTheme.shouldUseDarkColors
  const windowStatePath = join(userData, 'window-state.json')
  const placement = initialPlacement(await readWindowState(windowStatePath),
    { workAreas: screen.getAllDisplays().map(display => display.workArea), primary: screen.getPrimaryDisplay().workArea },
    { width: 1380, height: 920 }, { width: 950, height: 650 })
  const window = new BrowserWindow({ title: 'RainyAgent', ...placement.bounds, minWidth: 950, minHeight: 650, icon, show: false,
    titleBarStyle: 'hidden',
    titleBarOverlay: { height: CAPTION_HEIGHT, color: '#00000000', symbolColor: dark ? '#eeeeee' : '#171717' },
    backgroundColor: dark ? WINDOW_BACKGROUND.dark : WINDOW_BACKGROUND.light,
    webPreferences: { nodeIntegration: false, contextIsolation: true, sandbox: true, preload: preloadPath } })
  mainWindowCreated = true
  window.setMenuBarVisibility(false)
  // Geometry is tracked as it changes because a page-initiated close destroys the window without a 'close' event.
  let geometry: WindowState | undefined
  const trackGeometry = () => {
    if (!window.isMinimized()) geometry = { version: 1, bounds: window.getNormalBounds(), maximized: window.isMaximized() }
  }
  window.on('resize', trackGeometry).on('move', trackGeometry).on('maximize', trackGeometry).on('unmaximize', trackGeometry)
  window.on('closed', () => {
    if (geometry === undefined) return
    try { writeWindowState(windowStatePath, geometry) }
    catch (error) { appendFileSync(logPath, `Window state was not saved: ${errorText(error)}\n`) }
  })
  await window.loadURL(startupPage(dark))
  // The first visible frame is the startup page rather than an empty window.
  if (placement.maximized) window.maximize()
  window.show()
  const progress = startupProgress(window)
  timeline.mark('window')
  let integrity: ReleaseIntegrity | undefined
  if (app.isPackaged) {
    const keys = embeddedReleaseKeys()
    if (!keys) throw new Error('发行包缺少资源验证公钥，请重新安装完整发行包。')
    integrity = await ensureReleaseIntegrity({ root: resourceRoot, signedPath: join(resourceRoot, 'release-manifest.signed.json'), keys,
      stampPath: join(userData, 'release-verified.json'), recheckIntervalMs: RELEASE_RECHECK_INTERVAL_MS,
      onProgress: (completed, total) => {
        progress.step(`首次启动，正在校验安装文件 ${completed.toLocaleString('zh-CN')} / ${total.toLocaleString('zh-CN')}`, completed / total)
      } })
    timeline.mark(integrity.verified === 'full' ? 'verify (full)' : 'verify (stamp)')
  }
  const modulesRoot = join(userData, 'modules')
  const modules = new OptionalModules({ descriptorPath: join(resourceRoot, 'optional-modules.json'), root: modulesRoot,
    fetch: (input, init) => window.webContents.session.fetch(input instanceof URL ? input.href : input, init),
    publish: (state) => { if (!window.isDestroyed()) window.webContents.send('rainy:modules-progress', state) } })
  closeModules = () => modules.close()
  // A development checkout may still hold the engine that earlier releases bundled.
  const checkoutStrata = resolve(__dirname, '../resources/strata-runtime')
  const strata = createStrataManager({
    runtimeRoot: !app.isPackaged && existsSync(checkoutStrata) ? checkoutStrata : join(modulesRoot, 'strata'),
    userData,
  })
  closeStrata = () => strata.close()
  const environmentSetup = { installRoot, userData, settingsPath, preloadPath,
    pagePath: join(__dirname, 'setup/index.html'), mediaRoot: join(resourceRoot, 'environment'), icon, parent: window }
  const carrierState = join(userData, 'carrier-state')
  mkdirSync(carrierState, { recursive: true })
  const uvx = await uvxLookup
  const runtimeFiles = {
    metadata: join(resourceRoot, 'linux-runtime.json'),
    installer: app.isPackaged ? join(resourceRoot, 'install-runtime.py') : resolve(__dirname, '../scripts/install-runtime.py'),
  }
  // A development checkout stages the runtime archive itself.
  const linuxRuntimeArchive = async (): Promise<string> => !app.isPackaged && existsSync(join(resourceRoot, 'linux-runtime.tar.gz'))
    ? join(resourceRoot, 'linux-runtime.tar.gz') : modules.path('linux-runtime')
  /** Download this version's Linux runtime; failing that, the user retries, continues on Windows, or quits. */
  const downloadLinuxRuntime = async (): Promise<boolean> => {
    for (;;) {
      try {
        await modules.install('linux-runtime', (completed, total) => {
          progress.step(`正在下载 WSL 运行环境 ${Math.round(completed / 1024 ** 2)} / ${Math.round(total / 1024 ** 2)} MB`, completed / total)
        })
        return true
      } catch (error) {
        const answer = await dialog.showMessageBox(window, { type: 'warning', title: 'RainyAgent', message: 'WSL 运行环境下载失败',
          detail: `${errorText(error)}\n更新后第一次使用 WSL 时需要联网下载一次，已下载的部分会保留。`,
          buttons: ['重试', '改用 Windows 原生运行', '退出'], defaultId: 0, cancelId: 2 })
        if (answer.response === 1) return false
        if (answer.response !== 0) throw new Error('未下载 WSL 运行环境，RainyAgent 已停止启动。')
      }
    }
  }
  /** Map the carrier paths into the distribution and unpack its Host, with one wsl.exe call per step.
   * @returns the installed Host, or undefined when the user continues on Windows instead of downloading.
   */
  const prepareWsl = async (distribution: string) => {
    progress.step(`正在启动 WSL · ${distribution}…`)
    const [archive, metadata, installer, state, mappedUvx] = await wslPaths(distribution,
      [await linuxRuntimeArchive(), runtimeFiles.metadata, runtimeFiles.installer, carrierState, ...uvx === undefined ? [] : [uvx]])
    const install = async (): Promise<unknown> => JSON.parse((await run('wsl.exe', ['-d', distribution, '--exec', 'python3', installer, archive, metadata],
      { windowsHide: true, timeout: 180000 })).stdout)
    let installed = await install()
    if (installed !== null && typeof installed === 'object' && 'needsArchive' in installed) {
      if (!await downloadLinuxRuntime()) return undefined
      progress.step(`正在启动 WSL · ${distribution}…`)
      installed = await install()
      if (installed !== null && typeof installed === 'object' && 'needsArchive' in installed) throw new Error('下载的 WSL 运行环境与此版本不一致')
    }
    // The unpacked runtime is all WSL needs; only the next version downloads an archive again.
    if (app.isPackaged) {
      void modules.remove('linux-runtime').catch((error: unknown) => {
        appendFileSync(logPath, `The Linux runtime archive was not removed: ${errorText(error)}\n`)
      })
    }
    /** Delete the runtimes earlier versions unpacked; only the installed application does this, never a source checkout. */
    const pruneRuntimes = async (): Promise<string[]> => {
      const { stdout } = await run('wsl.exe', ['-d', distribution, '--exec', 'python3', installer, '--prune', metadata],
        { windowsHide: true, timeout: 600000 })
      const result: unknown = JSON.parse(stdout)
      return result !== null && typeof result === 'object' && 'removed' in result && Array.isArray(result.removed) ? result.removed.map(String) : []
    }
    return { installed, carrierState: state, uvx: mappedUvx, pruneRuntimes }
  }
  let prepared: Awaited<ReturnType<typeof prepareWsl>> | undefined
  let windowsInstead = false
  if (target.kind === 'wsl' && knownTarget) {
    try { prepared = await prepareWsl(requireWslDistribution(target)); windowsInstead = prepared === undefined }
    catch (error) { appendFileSync(logPath, `Recorded WSL target unavailable, checking the environment: ${errorText(error)}\n`) }
  }
  if (target.kind === 'wsl' && prepared === undefined && !windowsInstead) {
    progress.step('正在检查运行环境…')
    const environment = await prepareDesktopEnvironment(environmentSetup)
    target = (await listExecutionTargets()).find(value => value.kind === 'wsl' && value.distro === environment.distro)
      ?? { ...target, kind: 'wsl', distro: environment.distro, label: `WSL · ${environment.distro}` }
    environment.closeSetup()
    prepared = await prepareWsl(requireWslDistribution(target))
    windowsInstead = prepared === undefined
  }
  // Like a switch made in settings, continuing on Windows is recorded for later launches.
  if (windowsInstead) target = (await listExecutionTargets()).find(value => value.kind === 'windows') ?? target
  if (target.kind === 'wsl' && prepared === undefined) throw new Error('WSL 运行环境未就绪。')
  timeline.mark('runtime')
  const distro = target.kind === 'wsl' ? requireWslDistribution(target) : 'Windows'
  const convert = async (path: string) => target.kind === 'windows' ? path : wslPath(distro, '-u', path)
  const ipcDisposers: (() => void)[] = []
  const onIpc = (channel: string, listener: (event: Electron.IpcMainEvent, value: unknown) => void): void => {
    ipcMain.on(channel, listener)
    ipcDisposers.push(() => { ipcMain.removeListener(channel, listener) })
  }
  const handleIpc = (channel: string, handler: (event: Electron.IpcMainInvokeEvent, value: unknown) => unknown): void => {
    ipcMain.handle(channel, handler)
    ipcDisposers.push(() => { ipcMain.removeHandler(channel) })
  }
  let updateLocale: 'zh' | 'en' = app.getLocale().startsWith('zh') ? 'zh' : 'en'
  updates = new RainyUpdates(electronUpdater.autoUpdater, {
    enabled: app.isPackaged,
    notice: async (state, manual) => {
      if (window.isDestroyed() || lifecycle.closing) return
      window.setProgressBar(state.phase === 'downloading' ? state.percent / 100 : state.phase === 'checking' ? 2 : -1)
      const text = updateMessages[updateLocale]
      if (manual && (state.phase === 'current' || state.phase === 'error' || state.phase === 'unavailable')) {
        await dialog.showMessageBox(window, { type: state.phase === 'error' ? 'error' : 'info', title: text.title,
          message: state.phase === 'current' ? `${text.current} ${app.getVersion()}`
            : state.phase === 'unavailable' ? text.unavailable : text.failed,
          ...(state.phase === 'error' ? { detail: state.message } : {}) })
      }
    },
    confirm: async (version) => {
      if (window.isDestroyed() || lifecycle.closing) return false
      const text = updateMessages[updateLocale]
      const answer = await dialog.showMessageBox(window, { type: 'info', title: text.title,
        message: `${text.ready} ${version}`, detail: text.restartDetail,
        buttons: [text.later, text.install], defaultId: 0, cancelId: 0 })
      return answer.response === 1
    },
    restartWithInstall: async (install) => {
      const transition = { frozen: false, installed: false }
      try {
        await lifecycle.quit(false, {
          prepare: async () => {
            const text = updateMessages[updateLocale]
            if (componentInstallation || !transport || (await transport.inspectActivity('freeze')).active) {
              await dialog.showMessageBox(window, { type: 'info', title: text.title, message: text.busy })
              return false
            }
            transition.frozen = true
            return true
          },
          exit: () => {
            if (cleanupFailed) { app.quit(); return }
            transition.installed = true
            install()
          },
        })
        return transition.installed
      } finally {
        // A failed resume must not replace the installation outcome; a stopped Host has nothing frozen.
        if (transition.frozen && !lifecycle.quitting) {
          await transport?.inspectActivity('resume').catch((error: unknown) => { appendFileSync(logPath, `Host resume after update failed: ${errorText(error)}\n`) })
        }
      }
    },
  })
  const reloadAfterSave = createSavedReload({
    flush: () => flushWorkbench(),
    reload: (ignoreCache) => { if (ignoreCache) window.webContents.reloadIgnoringCache(); else window.webContents.reload() },
    isClosing: () => lifecycle.closing || window.isDestroyed() || window.webContents.isDestroyed(),
    reportFailure: async (error) => { await dialog.showMessageBox(window, { type: 'error', title: 'RainyAgent',
      message: '刷新前草稿尚未保存', detail: `${error}\n窗口和草稿已保留，请重新尝试刷新或导出草稿。` }) },
  })
  const requestReload = (ignoreCache = false): void => {
    void reloadAfterSave(ignoreCache).catch((error: unknown) => {
      void dialog.showMessageBox(window, { type: 'error', message: '刷新失败', detail: errorText(error) })
    })
  }
  window.webContents.on('before-input-event', (event, input) => {
    const ignoreCache = nativeReloadShortcut(input)
    if (ignoreCache === undefined) return
    event.preventDefault()
    requestReload(ignoreCache)
  })
  let switchTarget: (selected: ExecutionTarget, workspaceId?: WorkspaceId) => Promise<{ ok: boolean; error?: string }> =
    () => Promise.resolve({ ok: false, error: '执行环境仍在启动。' })
  const selectDistribution = async () => {
    const targets = await listExecutionTargets()
    const result = await dialog.showMessageBox(window, { message: '选择执行环境', detail: 'Windows 原生与 WSL 使用各自的解释器。已有聊天保留原执行环境；切换前请结束运行中的任务。',
      buttons: [...targets.map(value => value.label), '取消'], cancelId: targets.length })
    const selected = targets.find((_value, index) => index === result.response)
    if (selected && selected.id !== target.id) {
      const changed = await switchTarget(selected)
      if (!changed.ok) throw new Error(changed.error)
    }
  }
  const requestDistribution = (): void => {
    void selectDistribution().catch((error: unknown) => {
      void dialog.showMessageBox(window, { type: 'error', message: '切换发行版失败', detail: errorText(error) })
    })
  }
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: 'RainyAgent', submenu: [{ label: `执行环境：${target.label}`, click: requestDistribution },
      { label: '检查更新…', click: () => { void updates?.check(true) } }, { label: '退出', click: () => { app.quit() } }] },
    { label: '编辑', submenu: [{ role: 'undo' }, { role: 'redo' }, { type: 'separator' }, { role: 'cut' }, { role: 'copy' }, { role: 'paste' }, { role: 'selectAll' }] },
    { label: '视图', submenu: [{ label: '重新加载', accelerator: 'CmdOrCtrl+R', click: () => { requestReload() } }, { role: 'toggleDevTools' }, { role: 'resetZoom' }, { role: 'zoomIn' }, { role: 'zoomOut' }] },
  ]))
  let installed: unknown
  if (prepared !== undefined) installed = prepared.installed
  else {
    const root = join(resourceRoot, 'windows-host')
    const node = join(root, 'node', 'node.exe')
    const host = join(root, 'app', 'lib', 'host.js')
    if (!existsSync(node) || !existsSync(host)) throw new Error('Windows 原生运行文件缺失，请安装完整发行包。')
    installed = { node, host }
  }
  if (installed === null || typeof installed !== 'object' || !('node' in installed) || typeof installed.node !== 'string'
    || !(target.kind === 'windows' ? win32.isAbsolute(installed.node) : posix.isAbsolute(installed.node))
    || !('host' in installed) || typeof installed.host !== 'string'
    || !(target.kind === 'windows' ? win32.isAbsolute(installed.host) : posix.isAbsolute(installed.host))) throw new Error('执行环境运行文件无效。')
  const idaMcpCommand = prepared === undefined ? uvx : prepared.uvx
  // The PHP component is downloaded from settings; the Host notices it at its fixed path without restarting.
  const checkoutPhp = resolve(__dirname, '../runtime/component-stage/windows-basic/php/php.exe')
  const builtinPhp = !app.isPackaged && existsSync(checkoutPhp) ? checkoutPhp : join(modulesRoot, 'php', 'php.exe')
  const hostEnvironment: Record<string, string> = {
    RAINY_EXECUTION_TARGET_ID: target.id,
    RAINY_CARRIER_STATE_ROOT: prepared === undefined ? carrierState : prepared.carrierState,
    ...(target.kind === 'windows' ? { RAINY_HOME: join(userData, 'native-home'),
      RAINY_TOOLCHAIN_ROOT: join(userData, 'env'),
      RAINY_PWSH_PATH: join(resourceRoot, 'windows-host', 'pwsh', 'pwsh.exe'),
      RAINY_BUILTIN_PHP: builtinPhp,
      // Module compilation is cached across launches; resolution still dominates the native Host's startup.
      NODE_COMPILE_CACHE: join(userData, 'node-compile-cache') } : {}),
  }
  const pending = preferences.pendingProject
  if (pending !== null && typeof pending === 'object' && 'projectId' in pending && typeof pending.projectId === 'string'
    && 'path' in pending && typeof pending.path === 'string') {
    hostEnvironment.RAINY_PENDING_PROJECT_ID = pending.projectId
    hostEnvironment.RAINY_PENDING_PROJECT_PATH = pending.path
    if ('roots' in pending && Array.isArray(pending.roots)) hostEnvironment.RAINY_PENDING_PROJECT_ROOTS = JSON.stringify(pending.roots)
  }
  const hostOptions: HostOptions = { entry: installed.host, node: installed.node, idaMcpCommand, environment: hostEnvironment,
    configureDeepSeek: process.env.RAINY_CONFIGURE_DEEPSEEK === '1',
    onDiagnostic: (text) =>{  appendFileSync(logPath, text + '\n') },
    onExit: (code) => {
      if (lifecycle.quitting || code === 0 || window.isDestroyed()) return
      void dialog.showMessageBox(window, { type: 'error', message: '执行后端已停止',
        detail: `${code === null ? '进程已被终止' : `退出代码：${code}`}。会话历史已保留。` })
    },
  }
  transport = target.kind === 'windows' ? new WindowsHostTransport({ ...hostOptions, cwd: installRoot }) : new WslHostTransport({ ...hostOptions, distro })
  progress.step('正在启动执行后端…')
  const ready = await transport.start()
  timeline.mark('host')
  // Recording the resolved target lets the next launch skip the registry lookup and the environment window.
  if ((preferences.pendingProject !== undefined && preferences.pendingProject !== null) || !sameTarget(preferences.executionTarget, target))
    await saveExecutionTarget(settingsPath, target)
  const origin = new URL(ready.url).origin
  const toolKeys = parseReleaseKeyring(JSON.parse(await readFile(app.isPackaged
    ? join(resourceRoot, 'native-tools-public-keys.json') : join(installRoot, 'resources/native-tools-public-keys.json'), 'utf8')))
  const nativeTools = installNativeTools({ window, origin,
    installRoot: app.isPackaged ? installRoot : join(installRoot, `toolpacks/stage-${app.getVersion()}`), userData: app.getPath('userData'),
    download: {
      keys: toolKeys, installRootLocked: app.isPackaged,
      channelPath: app.isPackaged ? join(resourceRoot, 'native-tools-channel.signed.json') : join(installRoot, 'toolpacks/native-tools-channel.v2.signed.json'),
    } })
  closeNativeTools = () => nativeTools.close()
  const trustedSender = (event: Pick<Electron.IpcMainEvent, 'sender' | 'senderFrame'>) => event.sender === window.webContents
    && event.senderFrame === window.webContents.mainFrame && new URL(event.senderFrame.url).origin === origin
  const ideEnvironment = target.kind === 'wsl' ? createIdeEnvironmentWindow({ parent: window, distro,
    resourceRoot: posix.resolve(posix.dirname(installed.host), '../resources/ide'),
    preloadPath, pagePath: join(__dirname, 'setup/ide.html'), icon,
  }) : undefined
  closeIdeEnvironment = () => ideEnvironment?.close() ?? Promise.resolve()
  const prepareComponents = async (): Promise<void> => {
    if (componentInstallation) return componentInstallation
    if (lifecycle.closing || lifecycle.switching) throw new Error('应用正在退出或切换执行环境，请稍后再准备运行环境。')
    const operation = async (): Promise<void> => {
      const choices = target.kind === 'wsl' ? ['导入离线组件', '准备 Ubuntu 开发工具', '取消'] : ['导入离线组件', '准备 WSL 环境', '取消']
      const action = await dialog.showMessageBox(window, { title: 'RainyAgent', message: '准备运行环境',
        detail: '可直接复用已安装环境。导入只写入 RainyAgent 自有目录，不修改已有 Python、Conda 或系统 PATH。', buttons: choices, cancelId: choices.length - 1 })
      if (action.response === choices.length - 1) return
      if (ideEnvironment !== undefined && action.response === 1) { await ideEnvironment.open(); return }
      if (target.kind === 'windows' && action.response === 1) {
        const prepared = await prepareDesktopEnvironment(environmentSetup)
        prepared.closeSetup()
        await dialog.showMessageBox(window, { title: 'RainyAgent', message: 'WSL 环境已就绪', detail: '在运行环境页选择该 WSL 环境即可切换。' })
        return
      }
      const selected = await dialog.showOpenDialog(window, { title: '选择完整离线包中的组件 JSON', properties: ['openFile'], filters: [{ name: '离线组件清单', extensions: ['json'] }] })
      const descriptorPath = selected.filePaths[0]
      if (selected.canceled || !descriptorPath) return
      if ((await stat(descriptorPath)).size > 65536) throw new Error('离线组件清单过大。')
      const supplied = environmentComponentSchema.parse(JSON.parse(await readFile(descriptorPath, 'utf8')))
      const catalog = await readEnvironmentComponentCatalog(join(resourceRoot, 'environment-component-catalog.json'))
      const expected = catalog.find(value => value.id === supplied.id && value.sha256 === supplied.sha256
        && value.manifestSha256 === supplied.manifestSha256)
      if (!expected || expected.platform !== (target.kind === 'windows' ? 'windows' : 'linux')) throw new Error('该组件不属于当前发行包或执行环境。')
      if (target.kind === 'windows') await installWindowsComponent({ mediaDirectory: dirname(descriptorPath), root: join(app.getPath('userData'), 'env'), component: expected,
        progress: (message) => { if (!window.isDestroyed()) window.webContents.send('rainy:component-progress', { message }) } })
      else {
        const installerPath = await convert(app.isPackaged ? join(resourceRoot, 'install-environment-component.py') : join(installRoot, 'scripts/install-environment-component.py'))
        const archive = await convert(join(dirname(descriptorPath), expected.file))
        await run('wsl.exe', ['-d', distro, '--exec', 'python3', installerPath, archive, JSON.stringify(expected), '--root', posix.join(ready.home, 'components')],
          { windowsHide: true, timeout: 30 * 60 * 1000, maxBuffer: 1024 * 1024 })
      }
      await dialog.showMessageBox(window, { title: 'RainyAgent', message: '离线组件已校验并导入', detail: '点击运行环境页的“检测环境”，然后选择需要的解释器。' })
    }
    componentInstallation = operation().finally(() => { componentInstallation = undefined })
    return componentInstallation
  }
  let pendingFlush: {
    id: string
    timer: ReturnType<typeof setTimeout>
    promise: Promise<DraftFlushResult>
    resolve: (result: DraftFlushResult) => void
  } | undefined
  const flushed = (event: Electron.IpcMainEvent, value: unknown): void => {
    if (!trustedSender(event) || value === null || typeof value !== 'object' || !('id' in value) || value.id !== pendingFlush?.id
      || !('ok' in value) || typeof value.ok !== 'boolean' || pendingFlush === undefined) return
    clearTimeout(pendingFlush.timer)
    const request = pendingFlush
    pendingFlush = undefined
    request.resolve({ ok: value.ok, ...'error' in value && typeof value.error === 'string' ? { error: value.error } : {} })
  }
  onIpc('rainy:workbench-flushed', flushed)
  flushWorkbench = () => {
    if (pendingFlush !== undefined) return pendingFlush.promise
    const id = randomUUID()
    const { promise, resolve } = Promise.withResolvers<DraftFlushResult>()
    const timer = setTimeout(() => {
      pendingFlush = undefined
      resolve({ ok: false, error: '工具未能确认草稿已保存，请重试。' })
    }, 65000)
    pendingFlush = { id, timer, promise, resolve }
    try { window.webContents.send('rainy:flush-workbench', id) }
    catch (error) {
      clearTimeout(timer)
      pendingFlush = undefined
      resolve({ ok: false, error: errorText(error) })
    }
    return promise
  }
  switchTarget = (selected, workspaceId) => {
    if (selected.id === target.id) return Promise.resolve({ ok: true })
    return lifecycle.switchTarget(async () => {
      if (componentInstallation) return { ok: false, error: '请等待运行环境准备完成后再切换。' }
      if (!transport) return { ok: false, error: '应用正在退出或执行环境尚未就绪。' }
      if ((await transport.inspectActivity()).active) return { ok: false, error: '请先结束运行中的 AI 任务、程序、调试及终端，再切换执行环境。' }
      const saved = await flushWorkbench()
      if (!saved.ok) return { ok: false, error: saved.error ?? '草稿未保存，执行环境保持不变。' }
      workspaceId ??= (await transport.inspectProject())?.workspaceId
      let pendingProject: PendingProjectTarget | undefined
      const projects = createProjectRegistry({ root: carrierState, targetId: target.id })
      if (workspaceId) {
        const snapshot = await transport.inspectProject(workspaceId)
        if (!snapshot) throw new Error('The selected project is unavailable for target migration.')
        const records = await projects.list()
        const project = records.find(item => item.projectId === snapshot.projectId)
        if (!project) return { ok: false, error: '当前项目尚未登记，请刷新运行环境页面后重试。' }
        const current = project.bindings.find(binding => binding.targetId === target.id && binding.workspaceId === workspaceId)
        if (!current) throw new Error('The current project binding is unavailable.')
        // Windows paths are the shared form between the current and the selected target.
        const mapToSelected = async (path: string): Promise<string> => {
          const windowsPath = target.kind === 'windows' ? path : await wslPath(distro, '-w', path)
          if (!win32.isAbsolute(windowsPath)) throw new Error('项目路径无法映射到目标环境，请保留原环境。')
          return selected.kind === 'windows' ? windowsPath : wslPath(requireWslDistribution(selected), '-u', windowsPath)
        }
        const path = project.bindings.find(binding => binding.targetId === selected.id)?.path || await mapToSelected(current.path)
        const roots = []
        for (const root of snapshot.roots.filter(value => !value.primary)) {
          roots.push({ rootId: root.rootId, path: await mapToSelected(root.path), title: root.title })
        }
        pendingProject = { projectId: project.projectId, path, roots }
      }
      if ((await transport.inspectActivity('freeze')).active) return { ok: false, error: '新的任务已开始，执行环境保持不变。' }
      const frozenTransport = transport
      return { ok: true, commit: () => saveExecutionTarget(settingsPath, selected, pendingProject),
        resume: async () => { await frozenTransport.inspectActivity('resume') } }
    })
  }
  window.on('close', (event) => {
    if (lifecycle.quitting) return
    event.preventDefault()
    quitAfterSave()
  })
  window.on('closed', () => {
    for (const dispose of ipcDisposers.splice(0)) dispose()
    if (pendingFlush !== undefined) {
      clearTimeout(pendingFlush.timer)
      pendingFlush.resolve({ ok: false, error: '窗口已关闭，无法确认草稿保存状态。' })
      pendingFlush = undefined
    }
    flushWorkbench = () => Promise.resolve({ ok: true })
  })
  onIpc('rainy:caption-colors', (event, value) => {
    if (!trustedSender(event)) return
    const colors = captionColors(value)
    if (!colors) return
    if (value !== null && typeof value === 'object' && 'locale' in value && (value.locale === 'zh' || value.locale === 'en')) {
      updateLocale = value.locale
    }
    window.setTitleBarOverlay({ ...colors, color: '#00000000', height: CAPTION_HEIGHT })
    if (value !== null && typeof value === 'object' && 'theme' in value
      && (value.theme === 'dark' || value.theme === 'light' || value.theme === 'system')) nativeTheme.themeSource = value.theme
  })
  onIpc('rainy:select-distribution', (event) => { if (trustedSender(event)) requestDistribution() })
  onIpc('rainy:native-menu', (event, value) => {
    if (!trustedSender(event) || value === null || typeof value !== 'object' || !('menu' in value) || !('locale' in value)
      || (value.menu !== 'edit' && value.menu !== 'help') || (value.locale !== 'zh' && value.locale !== 'en')) return
    updateLocale = value.locale
    const template = nativeMenuTemplate(value.menu, value.locale, () => {
      void dialog.showMessageBox(window, { title: 'RainyAgent', message: `RainyAgent ${app.getVersion()}`, detail: 'Develop by NCUCyberBase' })
    }, () => { void updates?.check(true) })
    Menu.buildFromTemplate(template).popup({ window })
  })
  const selectDirectory = createIdeDirectoryPicker(async () => {
    if (target.kind === 'windows') {
      const selected = await dialog.showOpenDialog(window, { properties: ['openDirectory', 'createDirectory'] })
      const path = selected.filePaths[0]
      return selected.canceled || !path ? null : { path, displayPath: path }
    }
    return chooseIdeDirectory(
      () => dialog.showOpenDialog(window, { properties: ['openDirectory', 'createDirectory'] }),
      convert,
    )
  })
  handleIpc('rainy:ide-directory', (event) => {
    if (!trustedSender(event)) throw new Error('Directory selection is available only in the RainyAgent window')
    return selectDirectory()
  })
  handleIpc('rainy:ide-prepare-development', async (event) => {
    if (!trustedSender(event)) throw new Error('Development setup is available only in the RainyAgent window')
    await prepareComponents()
  })
  handleIpc('rainy:runtime-targets', async (event) => {
    if (!trustedSender(event)) throw new Error('Runtime selection is available only in the RainyAgent window.')
    return { current: target, targets: await listExecutionTargets() }
  })
  handleIpc('rainy:runtime-switch', async (event, value) => {
    if (!trustedSender(event) || value === null || typeof value !== 'object' || !('targetId' in value) || typeof value.targetId !== 'string') throw new Error('Invalid execution target request.')
    const selected = (await listExecutionTargets()).find(item => item.id === value.targetId)
    if (!selected) throw new Error('The selected execution target is unavailable.')
    return switchTarget(selected, 'workspaceId' in value && typeof value.workspaceId === 'string' ? WorkspaceId(value.workspaceId) : undefined)
  })
  const admitStrata = (event: Electron.IpcMainInvokeEvent, mutate = false): void => {
    if (!trustedSender(event)) throw new Error('Strata controls are available only in the RainyAgent window.')
    if (mutate && (lifecycle.closing || lifecycle.switching)) throw new Error('请等待应用切换或退出完成后再操作 Strata。')
  }
  const admitModule = (event: Electron.IpcMainInvokeEvent): void => {
    if (!trustedSender(event)) throw new Error('Component downloads are available only in the RainyAgent window.')
  }
  const moduleId = (value: unknown): 'strata' | 'php' => {
    if (value !== 'strata' && value !== 'php') throw new Error('Invalid component.')
    return value
  }
  handleIpc('rainy:modules-list', (event) => { admitModule(event); return modules.status() })
  handleIpc('rainy:modules-state', (event) => { admitModule(event); return modules.progress() })
  handleIpc('rainy:modules-install', async (event, value) => { admitModule(event); await modules.install(moduleId(value)) })
  handleIpc('rainy:modules-remove', async (event, value) => {
    admitModule(event)
    const id = moduleId(value)
    // An owned engine keeps its files open; external servers are never stopped.
    if (id === 'strata') await strata.stop()
    await modules.remove(id)
  })
  handleIpc('rainy:modules-cancel', async (event) => { admitModule(event); await modules.cancel() })
  handleIpc('rainy:strata-status', (event) => { admitStrata(event); return strata.status() })
  handleIpc('rainy:strata-save', (event, value) => { admitStrata(event, true); return strata.save(value) })
  handleIpc('rainy:strata-start', (event) => { admitStrata(event, true); return strata.start() })
  handleIpc('rainy:strata-stop', (event) => { admitStrata(event, true); return strata.stop() })
  handleIpc('rainy:strata-select-model', async (event, kind) => {
    admitStrata(event, true)
    if (kind !== 'gguf' && kind !== 'mtp' && kind !== 'directory' && kind !== 'profile') throw new Error('Invalid local model picker.')
    const selected = await dialog.showOpenDialog(window, kind === 'directory'
      ? { properties: ['openDirectory'] }
      : { properties: ['openFile'], filters: kind === 'profile'
        ? [{ name: 'Strata JSON', extensions: ['json'] }] : [{ name: 'GGUF', extensions: ['gguf'] }] })
    return selected.canceled ? null : selected.filePaths[0] ?? null
  })
  const controlURL = new URL('/rainy/control', origin).href
  handleIpc('rainy:strata-connect', async (event) => {
    admitStrata(event, true)
    const connection = await strata.connection()
    const statusResponse = await window.webContents.session.fetch(controlURL, { credentials: 'include' })
    const status = await controlBody(statusResponse)
    if (!statusResponse.ok || status === null || typeof status !== 'object' || !('models' in status) || !Array.isArray(status.models)) {
      throw new Error('当前执行环境的模型设置不可用，请稍后重试。')
    }
    const previous: unknown = status.models.find((value: unknown) => value !== null && typeof value === 'object'
      && 'provider' in value && value.provider === 'rainy-strata')
    let approvedBudget: { maxTokens: number; expectedContextWindow: number } | undefined
    if (previous !== null && typeof previous === 'object' && 'maxTokens' in previous && typeof previous.maxTokens === 'number') {
      try { resolveBudget(connection.contextWindow, previous.maxTokens) }
      catch (_incompatibleBudget) {
        const maxTokens = resolveBudget(connection.contextWindow).outputTokens
        const answer = await dialog.showMessageBox(window, updateLocale === 'zh'
          ? { type: 'question', title: 'Strata 输出上限', message: `当前上下文为 ${connection.contextWindow}，原输出上限 ${previous.maxTokens} 过大。`,
            detail: `将输出上限调整为 ${maxTokens} 并连接？推理档位保持不变。`, buttons: ['取消', '调整并连接'], defaultId: 0, cancelId: 0 }
          : { type: 'question', title: 'Strata output limit', message: `The ${previous.maxTokens} output limit does not fit the ${connection.contextWindow} context.`,
            detail: `Set the output limit to ${maxTokens} and connect? The thinking setting stays unchanged.`, buttons: ['Cancel', 'Adjust and connect'], defaultId: 0, cancelId: 0 })
        if (answer.response !== 1) throw new Error(updateLocale === 'zh' ? '已取消连接，原模型设置保持不变。' : 'Connection cancelled. Model settings are unchanged.')
        approvedBudget = { maxTokens, expectedContextWindow: connection.contextWindow }
      }
    }
    const response = await window.webContents.session.fetch(controlURL, {
      method: 'POST', credentials: 'include', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ method: 'connect-strata', params: { baseURL: connection.baseURL, ...approvedBudget } }),
    })
    const value = await controlBody(response)
    if (value !== null && typeof value === 'object' && 'error' in value && typeof value.error === 'string') throw new Error(value.error)
    if (!response.ok || value === null || typeof value !== 'object' || !('result' in value)
      || value.result === null || typeof value.result !== 'object' || !('provider' in value.result) || !('model' in value.result)
      || typeof value.result.provider !== 'string' || typeof value.result.model !== 'string') throw new Error('Strata 模型配置未完成，请重新检查当前执行环境。')
    return { provider: value.result.provider, model: value.result.model }
  })
  writeFileSync(join(app.getPath('userData'), 'host.json'), JSON.stringify({ target, distro, pid: ready.pid, home: ready.home, origin, runtime: installed.host }, null, 2) + '\n')
  const mappedPaths = new Map<string, string>()
  // Dropped-file paths are resolved synchronously because the renderer bridge returns them inline.
  onIpc('rainy:path', (event, path) => {
    event.returnValue = ''
    if (!trustedSender(event) || typeof path !== 'string' || !win32.isAbsolute(path) || path.includes('\0')) return
    if (target.kind === 'windows') { event.returnValue = path; return }
    try {
      const mapped = mappedPaths.get(path) ?? execFileSync('wsl.exe', ['-d', distro, '--exec', 'wslpath', '-u', path], { windowsHide: true, encoding: 'utf8', timeout: 5000 }).trim()
      if (!posix.isAbsolute(mapped)) return
      mappedPaths.set(path, mapped)
      event.returnValue = mapped
    } catch (error) { appendFileSync(logPath, `A dropped file could not be mapped into WSL: ${errorText(error)}\n`) }
  })
  window.webContents.setWindowOpenHandler(({ url }) => { if (/^https?:/.test(url)) void shell.openExternal(url); return { action: 'deny' } })
  window.webContents.on('will-navigate', (event, url) => { if (new URL(url).origin !== origin) event.preventDefault() })
  window.webContents.session.setPermissionRequestHandler((_contents, permission, callback, details) => {
    const url = details.requestingUrl
    callback(allowsClipboardWrite(permission, url, origin))
  })
  window.webContents.on('page-title-updated', (event) => { event.preventDefault(); window.setTitle('RainyAgent') })
  progress.step('正在加载工作台…')
  await window.loadURL(ready.url)
  timeline.mark('workbench')
  appendFileSync(logPath, `RainyAgent startup (${target.label}): ${timeline.summary()}\n`)
  void updates.check()
  const background = setTimeout(() => {
    void runBackgroundChecks({ integrity, target, settingsPath, logPath, window, busy: () => lifecycle.closing || lifecycle.switching })
    void modules.clean().catch((error: unknown) => { appendFileSync(logPath, `Component cleanup failed: ${errorText(error)}
`) })
    if (app.isPackaged) {
      void removeRetiredResources(resourceRoot).then((names) => {
        if (names.length) appendFileSync(logPath, `Removed resources that earlier installers left behind: ${names.join(', ')}
`)
      }, (error: unknown) => { appendFileSync(logPath, `Retired resources were not removed: ${errorText(error)}
`) })
    }
    void nativeTools.maintain().then((bytes) => {
      if (bytes > 0) appendFileSync(logPath, `Removed ${Math.round(bytes / 1024 ** 2)} MiB of program files from tool upgrade backups\n`)
    })
    if (app.isPackaged && prepared !== undefined) {
      void prepared.pruneRuntimes().then((removed) => {
        if (removed.length) appendFileSync(logPath, `Removed ${removed.length} WSL runtimes that earlier versions unpacked\n`)
      }, (error: unknown) => { appendFileSync(logPath, `Earlier WSL runtimes were not removed: ${errorText(error)}\n`) })
    }
  }, BACKGROUND_CHECK_DELAY_MS)
  window.on('closed', () => { clearTimeout(background) })
}

/** Resource consistency and WSL target identity are confirmed after the workbench is usable. */
async function runBackgroundChecks(options: {
  readonly integrity: ReleaseIntegrity | undefined
  readonly target: ExecutionTarget
  readonly settingsPath: string
  readonly logPath: string
  readonly window: BrowserWindow
  readonly busy: () => boolean
}): Promise<void> {
  try { await options.integrity?.recheck() }
  catch (error) {
    appendFileSync(options.logPath, `Release resource recheck failed: ${errorText(error)}\n`)
    if (error instanceof ReleaseIntegrityError && !options.window.isDestroyed()) {
      await dialog.showMessageBox(options.window, { type: 'warning', title: 'RainyAgent', message: 'RainyAgent 安装文件已被修改或损坏',
        detail: `${error.message}\n下次启动时会重新完整校验；如果问题仍然存在，请重新安装 RainyAgent。` })
    }
  }
  if (options.target.kind !== 'wsl') return
  try {
    // A distribution re-registered under the same name receives a new identity; the next launch adopts it.
    const current = (await listExecutionTargets()).find(value => value.kind === 'wsl' && value.distro === options.target.distro)
    if (current === undefined || sameTarget(current, options.target) || options.busy()) return
    if (!sameTarget(savedExecutionTarget(await readDesktopPreferences(options.settingsPath)), options.target)) return
    await saveExecutionTarget(options.settingsPath, current)
  } catch (error) { appendFileSync(options.logPath, `Execution target refresh failed: ${errorText(error)}\n`) }
}

const maintenanceIndex = process.argv.indexOf('--rainy-install-tools')
if (maintenanceIndex >= 0) {
  const mediaDirectory = process.argv.at(maintenanceIndex + 1)
  void app.whenReady().then(async () => {
    if (mediaDirectory === undefined || !win32.isAbsolute(mediaDirectory)) { app.exit(2); return }
    app.exit(await runToolPackMaintenance(mediaDirectory, process.argv.includes('--rainy-tools-silent')))
  }).catch((error: unknown) => { console.error('Offline tool installation failed', error); app.exit(1) })
}
else if (!app.requestSingleInstanceLock()) app.quit()
else {
  app.on('will-quit', () => { updates?.dispose() })
  app.on('second-instance', () => {
    for (const win of BrowserWindow.getAllWindows()) { win.restore(); win.show(); win.focus() }
  })
  app.on('window-all-closed', () => { if (mainWindowCreated) app.quit() })
  app.on('before-quit', (event) => {
    if (lifecycle.quitting) return
    event.preventDefault()
    quitAfterSave()
  })
  void app.whenReady().then(start).catch(async (error: unknown) => {
    await dialog.showMessageBox({ type: 'error', message: 'RainyAgent 启动失败', detail: errorText(error) })
    app.quit()
  })
}
