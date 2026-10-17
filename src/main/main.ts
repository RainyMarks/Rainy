/** Rainy desktop carrier for one explicitly selected Host: native Windows, a WSL distribution, or a Linux server over SSH. */
import { app, BrowserWindow, crashReporter, dialog, Menu, screen, shell, ipcMain, nativeTheme, safeStorage } from 'electron'
import { currentLocale, followLocale, localized, type Locale } from '../shared/locale.ts'
import { mkdirSync, appendFileSync, createReadStream, existsSync, writeFileSync } from 'node:fs'
import { createServer } from 'node:net'
import { Transform, type Readable } from 'node:stream'
import { arch, freemem, release, totalmem, version } from 'node:os'
import { basename, dirname, join, posix, resolve, win32 } from 'node:path'
import { execFile, execFileSync } from 'node:child_process'
import { promisify } from 'node:util'
import { SshHostTransport, WslHostTransport, WindowsHostTransport } from './transport.ts'
import type { HostOptions, HostReady, HostTransport } from './transport.ts'
import { parseSshDestination, remotePython, runSsh, sshFailure, sshName } from './ssh.ts'
import { startAskpass, type Askpass } from './ssh-askpass.ts'
import { sshQuestions } from './ssh-prompt.ts'
import { CAPTION_HEIGHT, captionColors } from './window-chrome.ts'
import { nativeMenuTemplate } from './native-menus.ts'
import { createSavedReload, nativeReloadShortcut } from './saved-reload.ts'
import type { DraftFlushResult } from './saved-reload.ts'
import { createDesktopLifecycle } from './desktop-lifecycle.ts'
import electronUpdater from 'electron-updater'
import { RainyUpdates } from './updates.ts'
import { updateMessages } from './update-messages.ts'
import { createStrataManager } from './strata.ts'
import { resolveBudget } from '../shared/budget.ts'
import { randomUUID } from 'node:crypto'
import { prepareDesktopEnvironment } from './native-environment-window.ts'
import { OptionalModules } from './optional-modules.ts'
import { pruneWindowsHost, removeRetiredResources } from './retired-resources.ts'
import { chooseIdeDirectory, createIdeDirectoryPicker } from './ide-native.ts'
import { OfficeConversionError, OfficeConverter, officeKind } from './office-convert.ts'
import { createIdeEnvironmentWindow } from './ide-environment-window.ts'
import { listExecutionTargets, readDesktopPreferences, removeSshHost, savedExecutionTarget, saveDesktopLocale, saveExecutionTarget, saveSshHost,
  sshDestinationOf, sshTarget, sshTargets, WINDOWS_TARGET } from './execution-targets.ts'
import type { ExecutionTarget, PendingProjectTarget } from './execution-targets.ts'
import { createProjectRegistry } from '../shared/project-registry.ts'
import { environmentComponentSchema, installWindowsComponent, readEnvironmentComponentCatalog } from './environment-components.ts'
import { readFile, stat } from 'node:fs/promises'
import { embeddedReleaseKeys } from './release-trust.ts'
import { ensureReleaseIntegrity, ReleaseIntegrityError } from './release-integrity.ts'
import type { ReleaseIntegrity } from './release-integrity.ts'
import { startupPage, startupProgress, WINDOW_BACKGROUND } from './startup-splash.ts'
import { systemProxyFor } from './system-proxy.ts'
import { credentialKey } from './credential-key.ts'
import { exportDiagnostics, newestFiles } from './diagnostics.ts'
import { initialPlacement, readWindowState, writeWindowState } from './window-state.ts'
import type { WindowState } from './window-state.ts'
import type { WorkspaceId } from '../shared/ide-files-protocol.ts'
import { IDE_RAW_ROUTE } from '../shared/ide-preview.ts'
import { brandString } from '../shared/brand.ts'
import { allowsClipboardWrite } from './clipboard-policy.ts'

const run = promisify(execFile)
app.setName('Rainy')
app.setAppUserModelId('dev.rainy.agent')
const userDataDirectory = app.commandLine.getSwitchValue('user-data-dir')
app.setPath('userData', userDataDirectory ? resolve(userDataDirectory) : join(app.getPath('appData'), 'RainyAgent'))
// Crashes of the windows are recorded next to the other user data for the diagnostics export; nothing is uploaded.
app.setPath('crashDumps', join(app.getPath('userData'), 'Crashpad'))
crashReporter.start({ uploadToServer: false })
let transport: HostTransport | undefined
let flushWorkbench: () => Promise<DraftFlushResult> = () => Promise.resolve({ ok: true })
let closeIdeEnvironment: () => Promise<void> = () => Promise.resolve()
let closeStrata: () => Promise<void> = () => Promise.resolve()
let closeModules: () => Promise<void> = () => Promise.resolve()
let closeAskpass: () => Promise<void> = () => Promise.resolve()
let componentInstallation: Promise<void> | undefined
/** Read or created once per run; every Host started in this run encrypts API keys with it. */
let credentialKeyOnce: Promise<string | null> | undefined
/** @returns The interface language that matches the Windows display language. */
const systemLocale = (): Locale => app.getLocale().toLowerCase().startsWith('zh') ? 'zh' : 'en'
followLocale(() => app.isReady() ? systemLocale() : 'zh')
let updates: RainyUpdates | undefined
let cleanupFailed = false
// Setup windows may close before the workbench takes ownership of the application.
let mainWindowCreated = false
const lifecycle = createDesktopLifecycle({
  flush: () => flushWorkbench(),
  retrySave: async (error) => {
    const answer = await dialog.showMessageBox({ type: 'error', title: 'Rainy',
      message: localized({ zh: '草稿尚未保存', en: 'Your drafts have not been saved.' }),
      detail: error ?? localized({ zh: '保存失败，窗口和草稿已保留。', en: 'Saving failed. The window and your drafts are kept.' }),
      buttons: [localized({ zh: '重试保存并退出', en: 'Retry saving and quit' }), localized({ zh: '取消退出', en: 'Do not quit' })], defaultId: 0, cancelId: 1 })
    return answer.response === 0
  },
  cleanup: [async () => { await componentInstallation }, () => closeIdeEnvironment(),
    async () => { await transport?.stop() }, () => closeStrata(), () => closeModules(), () => closeAskpass()],
  reportCleanupFailure: (error) => { cleanupFailed = true; console.error('Rainy cleanup failed:', error) },
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
  if (lines.length !== paths.length || !lines.every(line => posix.isAbsolute(line))) {
    throw new Error(localized({ zh: 'WSL 路径转换结果无效。', en: 'WSL did not convert the paths correctly.' }))
  }
  return lines
}

/** @param value - persisted preference. @param target - resolved target. @returns whether the preference already records it. */
function sameTarget(value: unknown, target: ExecutionTarget): boolean {
  return value !== null && typeof value === 'object' && 'id' in value && value.id === target.id && 'kind' in value && value.kind === target.kind
    && 'label' in value && value.label === target.label && ('distro' in value ? value.distro : undefined) === target.distro
}

/** @returns a loopback port that was free a moment ago; the SSH Host binds the same number on the server. */
function freeLoopbackPort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer()
    server.once('error', reject)
    server.listen(0, '127.0.0.1', () => {
      const address = server.address()
      const port = address !== null && typeof address === 'object' ? address.port : 0
      server.close(() => { if (port > 0) resolve(port); else reject(new Error('No loopback port is free.')) })
    })
  })
}

/** Documents besides the Office formats that "Open in default app" accepts. */
const OPENABLE_DOCUMENTS = new Set(['pdf', 'xlsx', 'xlsm', 'xltx', 'xltm', 'xlsb', 'xls', 'xlt', 'ods', 'csv'])

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object'
}

/** @param bytes - a size. @returns whole megabytes, for progress text. */
const megabytes = (bytes: number): number => Math.round(bytes / 1024 ** 2)

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
    await dialog.showMessageBox({ type: 'error', message: localized({ zh: '退出前保存失败', en: 'Saving before quitting failed.' }), detail: errorText(error) })
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
  const preferences = await readDesktopPreferences(settingsPath)
  // Dialogs, menus and errors follow the interface language the page last reported; the system language before that.
  let interfaceLocale: Locale = preferences.locale === 'zh' || preferences.locale === 'en' ? preferences.locale : systemLocale()
  followLocale(() => interfaceLocale)
  const reportLocale = (locale: Locale): void => {
    if (locale === interfaceLocale) return
    interfaceLocale = locale
    void saveDesktopLocale(settingsPath, locale).catch((error: unknown) => { appendFileSync(logPath, `The interface language was not saved: ${errorText(error)}\n`) })
  }
  let target = savedExecutionTarget(preferences)
  // A launch that already recorded its resolved WSL target skips the environment window and the registry lookup.
  const knownTarget = target.kind === 'wsl' && preferences.executionTarget !== undefined
  // Booting the WSL VM overlaps with resource admission and the first paint.
  if (knownTarget) void run('wsl.exe', ['-d', requireWslDistribution(target), '--exec', 'true'], { windowsHide: true, timeout: 120000 })
    .catch((error: unknown) => { appendFileSync(logPath, `WSL warm-up failed: ${errorText(error)}\n`) })
  const preloadPath = join(__dirname, 'preload.cjs')
  const icon = app.isPackaged ? join(resourceRoot, 'icon.ico') : resolve(__dirname, '../build/icon.ico')
  const dark = nativeTheme.shouldUseDarkColors
  const windowStatePath = join(userData, 'window-state.json')
  const placement = initialPlacement(await readWindowState(windowStatePath),
    { workAreas: screen.getAllDisplays().map(display => display.workArea), primary: screen.getPrimaryDisplay().workArea },
    { width: 1380, height: 920 }, { width: 950, height: 650 })
  const window = new BrowserWindow({ title: 'Rainy', ...placement.bounds, minWidth: 950, minHeight: 650, icon, show: false,
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
    if (!keys) {
      throw new Error(localized({ zh: '发行包缺少资源验证公钥，请重新安装完整发行包。',
        en: 'This installation has no key to verify its files. Reinstall Rainy from the full installer.' }))
    }
    integrity = await ensureReleaseIntegrity({ root: resourceRoot, signedPath: join(resourceRoot, 'release-manifest.signed.json'), keys,
      stampPath: join(userData, 'release-verified.json'), recheckIntervalMs: RELEASE_RECHECK_INTERVAL_MS,
      onProgress: (completed, total) => {
        progress.step(localized({ zh: `首次启动，正在校验安装文件 ${completed.toLocaleString('zh-CN')} / ${total.toLocaleString('zh-CN')}`,
          en: `First start: verifying installed files ${completed.toLocaleString('en-US')} / ${total.toLocaleString('en-US')}` }), completed / total)
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
          progress.step(localized({ zh: `正在下载 Linux 运行环境 ${megabytes(completed)} / ${megabytes(total)} MB`,
            en: `Downloading the Linux runtime ${megabytes(completed)} / ${megabytes(total)} MB` }), completed / total)
        })
        return true
      } catch (error) {
        const answer = await dialog.showMessageBox(window, { type: 'warning', title: 'Rainy',
          message: localized({ zh: 'Linux 运行环境下载失败', en: 'The Linux runtime could not be downloaded.' }),
          detail: localized({ zh: `${errorText(error)}\n更新后第一次使用 WSL 或 SSH 远程开发时需要联网下载一次，已下载的部分会保留。`,
            en: `${errorText(error)}\nAfter an update, WSL and SSH remote development need a one-time download the first time you use them. What was already downloaded is kept.` }),
          buttons: [localized({ zh: '重试', en: 'Retry' }), localized({ zh: '改用 Windows 原生运行', en: 'Run on Windows instead' }), localized({ zh: '退出', en: 'Quit' })],
          defaultId: 0, cancelId: 2 })
        if (answer.response === 1) return false
        if (answer.response !== 0) {
          throw new Error(localized({ zh: '未下载 Linux 运行环境，Rainy 已停止启动。', en: 'Rainy did not start because the Linux runtime was not downloaded.' }))
        }
      }
    }
  }
  /** Map the carrier paths into the distribution and unpack its Host, with one wsl.exe call per step.
   * @returns the installed Host, or undefined when the user continues on Windows instead of downloading.
   */
  const prepareWsl = async (distribution: string) => {
    const startingWsl = (): string => localized({ zh: `正在启动 WSL · ${distribution}…`, en: `Starting WSL · ${distribution}…` })
    progress.step(startingWsl())
    const [archive, metadata, installer, state] = await wslPaths(distribution,
      [await linuxRuntimeArchive(), runtimeFiles.metadata, runtimeFiles.installer, carrierState])
    const install = async (): Promise<unknown> => JSON.parse((await run('wsl.exe', ['-d', distribution, '--exec', 'python3', installer, archive, metadata],
      { windowsHide: true, timeout: 180000 })).stdout)
    let installed = await install()
    if (installed !== null && typeof installed === 'object' && 'needsArchive' in installed) {
      if (!await downloadLinuxRuntime()) return undefined
      progress.step(startingWsl())
      installed = await install()
      if (installed !== null && typeof installed === 'object' && 'needsArchive' in installed) {
        throw new Error(localized({ zh: '下载的 WSL 运行环境与此版本不一致', en: 'The downloaded WSL runtime does not match this version of Rainy.' }))
      }
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
    return { installed, carrierState: state, pruneRuntimes }
  }
  let prepared: Awaited<ReturnType<typeof prepareWsl>> | undefined
  let windowsInstead = false
  if (target.kind === 'wsl' && knownTarget) {
    try { prepared = await prepareWsl(requireWslDistribution(target)); windowsInstead = prepared === undefined }
    catch (error) { appendFileSync(logPath, `Recorded WSL target unavailable, checking the environment: ${errorText(error)}\n`) }
  }
  if (target.kind === 'wsl' && prepared === undefined && !windowsInstead) {
    progress.step(localized({ zh: '正在检查运行环境…', en: 'Checking the runtime environment…' }))
    const environment = await prepareDesktopEnvironment(environmentSetup)
    target = (await listExecutionTargets()).find(value => value.kind === 'wsl' && value.distro === environment.distro)
      ?? { ...target, kind: 'wsl', distro: environment.distro, label: `WSL · ${environment.distro}` }
    environment.closeSetup()
    prepared = await prepareWsl(requireWslDistribution(target))
    windowsInstead = prepared === undefined
  }
  const askpassNode = join(resourceRoot, 'windows-host', 'node', 'node.exe')
  let askpass: Promise<Askpass> | undefined
  /** Askpass of one ssh run, so ssh asks its questions in Rainy's windows; none without the bundled node.exe. */
  const askpassSession = async () => {
    if (!existsSync(askpassNode)) return undefined
    askpass ??= startAskpass({ node: askpassNode, directory: userData, ask: sshQuestions(() => window.isDestroyed() ? undefined : window) })
    const server = await askpass
    closeAskpass = () => server.close()
    return server.session()
  }
  /** Run the runtime installer on an SSH server and return its JSON answer. */
  const sshInstaller = async (selected: ExecutionTarget, args: readonly string[], input?: Readable): Promise<unknown> => {
    const destination = sshDestinationOf(selected)
    const session = await askpassSession()
    try {
      const [installer, metadata] = await Promise.all([readFile(runtimeFiles.installer, 'utf8'), readFile(runtimeFiles.metadata, 'utf8')])
      const result = await runSsh(destination, remotePython(installer, [args[0]!, metadata, ...args.slice(1)]), {
        ...(session === undefined ? {} : { environment: session.environment }), ...(input === undefined ? {} : { input }),
        onErrorLine: (line) => {
          const fetched = /^RAINY_PROGRESS (\d+) (\d+)$/u.exec(line)
          if (fetched === null) { appendFileSync(logPath, `ssh ${selected.label}: ${line}\n`); return }
          const [completed, total] = [Number(fetched[1]), Number(fetched[2])]
          progress.step(localized({ zh: `服务器正在下载 Linux 运行环境 ${megabytes(completed)} / ${megabytes(total)} MB`,
            en: `The server is downloading the Linux runtime ${megabytes(completed)} / ${megabytes(total)} MB` }), total > 0 ? completed / total : undefined)
        } })
      if (result.code !== 0) {
        if (/Permission denied/u.test(result.stderr)) session?.failed()
        throw sshFailure(destination, result.code, result.stderr)
      }
      try { return JSON.parse(result.stdout.trim().split('\n').at(-1) ?? '') as unknown }
      catch (_malformed) {
        throw new Error(localized({ zh: '服务器返回的安装结果无效。', en: 'The server returned an invalid installation result.' }) + `\n${result.stdout.slice(-400)}`)
      }
    } finally { session?.dispose() }
  }
  /** Install this version's Linux runtime on the server: as it is, downloaded there, or uploaded from here.
   * @returns the installed Host, or undefined when the user continues on Windows instead of downloading.
   */
  const prepareSsh = async (selected: ExecutionTarget): Promise<{ node: string; host: string } | undefined> => {
    const name = sshName(sshDestinationOf(selected))
    progress.step(localized({ zh: `正在连接 ${selected.label}…`, en: `Connecting to ${selected.label}…` }))
    const pieces = process.env.RAINY_SSH_NO_FETCH === '1' ? [] : await modules.pieces('linux-runtime')
    let result = await sshInstaller(selected, ['--fetch', JSON.stringify(pieces)])
    if (isRecord(result) && 'needsArchive' in result) {
      if ('error' in result) appendFileSync(logPath, `${selected.label} could not download the Linux runtime itself: ${String(result.error)}\n`)
      const developmentArchive = !app.isPackaged && existsSync(join(resourceRoot, 'linux-runtime.tar.gz'))
      if (!developmentArchive && !await downloadLinuxRuntime()) return undefined
      const archive = await linuxRuntimeArchive()
      const size = (await stat(archive)).size
      let sent = 0
      // Counting in the pipeline, not with a 'data' listener, which would start reading before ssh is ready.
      const input = createReadStream(archive).pipe(new Transform({ transform(chunk: Buffer, _encoding, done) {
        sent += chunk.length
        progress.step(localized({ zh: `正在上传 Linux 运行环境到 ${name} ${megabytes(sent)} / ${megabytes(size)} MB`,
          en: `Uploading the Linux runtime to ${name} ${megabytes(sent)} / ${megabytes(size)} MB` }), sent / size)
        done(null, chunk)
      } }))
      result = await sshInstaller(selected, ['--receive'], input)
      if (isRecord(result) && 'needsArchive' in result) {
        throw new Error(localized({ zh: `上传到 ${name} 的 Linux 运行环境校验失败，连接可能中途断开了。`, en: `The Linux runtime uploaded to ${name} failed verification; the connection may have dropped.` }))
      }
      if (app.isPackaged) {
        void modules.remove('linux-runtime').catch((error: unknown) => { appendFileSync(logPath, `The Linux runtime archive was not removed: ${errorText(error)}\n`) })
      }
    }
    if (isRecord(result) && 'unsupported' in result) {
      throw new Error(localized({ zh: `${name} 是 ${String(result.unsupported)}；SSH 远程开发目前只支持 x86_64 Linux 服务器。`,
        en: `${name} runs ${String(result.unsupported)}; SSH remote development supports x86_64 Linux servers only.` }))
    }
    if (!isRecord(result) || typeof result.node !== 'string' || typeof result.host !== 'string') {
      throw new Error(localized({ zh: `${name} 上的 Linux 运行环境没有安装成功。`, en: `The Linux runtime was not installed on ${name}.` }))
    }
    return { node: result.node, host: result.host }
  }
  /** A failed SSH step offers to retry, to continue on Windows when that is still possible, or to quit. */
  const sshStep = async <T,>(selected: ExecutionTarget, step: () => Promise<T>, windowsAllowed: boolean): Promise<T | undefined> => {
    for (;;) {
      try { return await step() }
      catch (error) {
        appendFileSync(logPath, `${selected.label}: ${errorText(error)}\n`)
        const buttons = [localized({ zh: '重试', en: 'Retry' }), ...windowsAllowed ? [localized({ zh: '改用 Windows 原生运行', en: 'Run on Windows instead' })] : [],
          localized({ zh: '退出', en: 'Quit' })]
        const answer = await dialog.showMessageBox(window, { type: 'warning', title: 'Rainy', noLink: true, buttons, defaultId: 0, cancelId: buttons.length - 1,
          message: localized({ zh: `无法连接 ${selected.label}`, en: `Rainy cannot connect to ${selected.label}` }), detail: errorText(error) })
        if (answer.response === 0) continue
        if (windowsAllowed && answer.response === 1) return undefined
        throw new Error(localized({ zh: `未连接 ${selected.label}，Rainy 已停止启动。`, en: `Rainy did not start because it could not connect to ${selected.label}.` }))
      }
    }
  }
  let sshInstalled: { node: string; host: string } | undefined
  if (target.kind === 'ssh') {
    const selected = target
    sshInstalled = await sshStep(selected, () => prepareSsh(selected), true)
    windowsInstead = sshInstalled === undefined
  }
  // Like a switch made in settings, continuing on Windows is recorded for later launches.
  if (windowsInstead) target = WINDOWS_TARGET
  if (target.kind === 'wsl' && prepared === undefined) throw new Error(localized({ zh: 'WSL 运行环境未就绪。', en: 'The WSL runtime is not ready.' }))
  timeline.mark('runtime')
  const distro = target.kind === 'wsl' ? requireWslDistribution(target) : target.kind === 'ssh' ? target.label : 'Windows'
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
  updates = new RainyUpdates(electronUpdater.autoUpdater, {
    enabled: app.isPackaged,
    notice: async (state, manual) => {
      if (window.isDestroyed() || lifecycle.closing) return
      window.setProgressBar(state.phase === 'downloading' ? state.percent / 100 : state.phase === 'checking' ? 2 : -1)
      const text = updateMessages[currentLocale()]
      if (manual && (state.phase === 'current' || state.phase === 'error' || state.phase === 'unavailable')) {
        await dialog.showMessageBox(window, { type: state.phase === 'error' ? 'error' : 'info', title: text.title,
          message: state.phase === 'current' ? `${text.current} ${app.getVersion()}`
            : state.phase === 'unavailable' ? text.unavailable : text.failed,
          ...(state.phase === 'error' ? { detail: state.message } : {}) })
      }
    },
    confirm: async (version) => {
      if (window.isDestroyed() || lifecycle.closing) return false
      const text = updateMessages[currentLocale()]
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
            const text = updateMessages[currentLocale()]
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
    reportFailure: async (error) => { await dialog.showMessageBox(window, { type: 'error', title: 'Rainy',
      message: localized({ zh: '刷新前草稿尚未保存', en: 'Your drafts could not be saved before reloading.' }),
      detail: localized({ zh: `${error}\n窗口和草稿已保留，请重新尝试刷新或导出草稿。`,
        en: `${error}\nThe window and your drafts are kept. Try reloading again, or export your drafts.` }) }) },
  })
  const requestReload = (ignoreCache = false): void => {
    void reloadAfterSave(ignoreCache).catch((error: unknown) => {
      void dialog.showMessageBox(window, { type: 'error', message: localized({ zh: '刷新失败', en: 'Reloading failed.' }), detail: errorText(error) })
    })
  }
  window.webContents.on('before-input-event', (event, input) => {
    const ignoreCache = nativeReloadShortcut(input)
    if (ignoreCache === undefined) return
    event.preventDefault()
    requestReload(ignoreCache)
  })
  let switchTarget: (selected: ExecutionTarget, workspaceId?: WorkspaceId) => Promise<{ ok: boolean; error?: string }> =
    () => Promise.resolve({ ok: false, error: localized({ zh: '执行环境仍在启动。', en: 'The execution environment is still starting.' }) })
  /** Windows, the WSL distributions and the SSH hosts; `saved` marks hosts added in Rainy. */
  const availableTargets = async (): Promise<(ExecutionTarget & { saved?: boolean })[]> => {
    const [local, remote] = await Promise.all([listExecutionTargets(), sshTargets(await readDesktopPreferences(settingsPath))])
    return [...local, ...remote]
  }
  const findTarget = async (id: string): Promise<ExecutionTarget | undefined> => {
    const found = (await availableTargets()).find(item => item.id === id)
    if (found === undefined) return undefined
    const { saved: _saved, ...selected } = found
    return selected
  }
  const selectDistribution = async () => {
    const targets = await availableTargets()
    const result = await dialog.showMessageBox(window, { message: localized({ zh: '选择执行环境', en: 'Choose an execution environment' }),
      detail: localized({ zh: 'Windows 原生与 WSL 使用各自的解释器。已有聊天保留原执行环境；切换前请结束运行中的任务。',
        en: 'Windows native and WSL each use their own interpreters. Existing chats keep their execution environment. Finish running tasks before you switch.' }),
      buttons: [...targets.map(value => value.label), localized({ zh: '取消', en: 'Cancel' })], cancelId: targets.length })
    const chosen = targets.find((_value, index) => index === result.response)
    const selected = chosen === undefined ? undefined : await findTarget(chosen.id)
    if (selected && selected.id !== target.id) {
      const changed = await switchTarget(selected)
      if (!changed.ok) throw new Error(changed.error)
    }
  }
  const requestDistribution = (): void => {
    void selectDistribution().catch((error: unknown) => {
      void dialog.showMessageBox(window, { type: 'error', message: localized({ zh: '切换发行版失败', en: 'Switching the distribution failed.' }), detail: errorText(error) })
    })
  }
  // This menu bar is never shown (hidden title bar, setMenuBarVisibility(false), no Alt toggle) and only carries shortcuts,
  // so its labels follow the language at launch and are not rebuilt when the language changes.
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: 'Rainy', submenu: [{ label: localized({ zh: `执行环境：${target.label}`, en: `Execution environment: ${target.label}` }), click: requestDistribution },
      { label: localized({ zh: '检查更新…', en: 'Check for updates…' }), click: () => { void updates?.check(true) } },
      { label: localized({ zh: '退出', en: 'Quit' }), click: () => { app.quit() } }] },
    { label: localized({ zh: '编辑', en: 'Edit' }),
      submenu: [{ role: 'undo' }, { role: 'redo' }, { type: 'separator' }, { role: 'cut' }, { role: 'copy' }, { role: 'paste' }, { role: 'selectAll' }] },
    { label: localized({ zh: '视图', en: 'View' }), submenu: [{ label: localized({ zh: '重新加载', en: 'Reload' }), accelerator: 'CmdOrCtrl+R', click: () => { requestReload() } },
      { role: 'toggleDevTools' }, { role: 'resetZoom' }, { role: 'zoomIn' }, { role: 'zoomOut' }] },
  ]))
  let installed: unknown
  if (prepared !== undefined) installed = prepared.installed
  else if (sshInstalled !== undefined) installed = sshInstalled
  else {
    const root = join(resourceRoot, 'windows-host')
    const node = join(root, 'node', 'node.exe')
    const host = join(root, 'app', 'dist', 'host.js')
    if (!existsSync(node) || !existsSync(host)) {
      throw new Error(localized({ zh: 'Windows 原生运行文件缺失，请安装完整发行包。', en: 'The files for running natively on Windows are missing. Install the full Rainy package.' }))
    }
    installed = { node, host }
  }
  if (installed === null || typeof installed !== 'object' || !('node' in installed) || typeof installed.node !== 'string'
    || !(target.kind === 'windows' ? win32.isAbsolute(installed.node) : posix.isAbsolute(installed.node))
    || !('host' in installed) || typeof installed.host !== 'string'
    || !(target.kind === 'windows' ? win32.isAbsolute(installed.host) : posix.isAbsolute(installed.host))) {
    throw new Error(localized({ zh: '执行环境运行文件无效。', en: 'The runtime files of the execution environment are invalid.' }))
  }
  // The PHP component is downloaded from settings; the Host notices it at its fixed path without restarting.
  const checkoutPhp = resolve(__dirname, '../runtime/component-stage/windows-basic/php/php.exe')
  const builtinPhp = !app.isPackaged && existsSync(checkoutPhp) ? checkoutPhp : join(modulesRoot, 'php', 'php.exe')
  const hostEnvironment: Record<string, string> = {
    RAINY_EXECUTION_TARGET_ID: target.id,
    RAINY_EXECUTION_TARGET_LABEL: target.label,
    // The Host asks this process for the key that encrypts its API keys.
    RAINY_CARRIER_CREDENTIALS: '1',
    // An SSH server keeps its project records and memory in its own home.
    ...(target.kind === 'ssh' ? {} : { RAINY_CARRIER_STATE_ROOT: prepared === undefined ? carrierState : prepared.carrierState }),
    ...(target.kind === 'windows' ? { RAINY_HOME: join(userData, 'native-home'),
      RAINY_TOOLCHAIN_ROOT: join(userData, 'env'),
      RAINY_BUILTIN_PHP: builtinPhp,
      // Module compilation is cached across launches; resolution still dominates the native Host's startup.
      NODE_COMPILE_CACHE: join(userData, 'node-compile-cache'),
      // Model requests follow the Windows system proxy, which the Host asks this process for; WSL keeps its own network.
      RAINY_CARRIER_PROXY: '1' } : {}),
  }
  const pending = preferences.pendingProject
  if (target.kind !== 'ssh' && pending !== null && typeof pending === 'object' && 'projectId' in pending && typeof pending.projectId === 'string'
    && 'path' in pending && typeof pending.path === 'string') {
    hostEnvironment.RAINY_PENDING_PROJECT_ID = pending.projectId
    hostEnvironment.RAINY_PENDING_PROJECT_PATH = pending.path
    if ('roots' in pending && Array.isArray(pending.roots)) hostEnvironment.RAINY_PENDING_PROJECT_ROOTS = JSON.stringify(pending.roots)
  }
  let hostReady = false
  /** The last lines ssh wrote, to explain a connection that closed during startup. */
  const sshSaid: string[] = []
  const hostOptions: HostOptions = { entry: installed.host, node: installed.node, environment: hostEnvironment,
    resolveProxy: url => systemProxyFor(window.webContents.session, url),
    credentialKey: () => credentialKeyOnce ??= credentialKey(userData, safeStorage, (line) => { appendFileSync(logPath, `${line}\n`) }),
    configureDeepSeek: process.env.RAINY_CONFIGURE_DEEPSEEK === '1',
    onDiagnostic: (text) => {
      appendFileSync(logPath, text + '\n')
      if (target.kind === 'ssh') { sshSaid.push(text); if (sshSaid.length > 40) sshSaid.shift() }
    },
    onExit: (code) => {
      if (!hostReady || lifecycle.quitting || code === 0 || window.isDestroyed()) return
      if (target.kind === 'ssh') {
        void dialog.showMessageBox(window, { type: 'warning', title: 'Rainy', noLink: true, defaultId: 0, cancelId: 1,
          message: localized({ zh: `与 ${target.label} 的连接已断开`, en: `The connection to ${target.label} was lost` }),
          detail: localized({ zh: '远程后端随连接一起停止了，会话历史保存在服务器上。重新连接会重新打开 Rainy。',
            en: 'The remote backend stopped with the connection. Chat history is kept on the server. Reconnecting reopens Rainy.' }),
          buttons: [localized({ zh: '重新连接', en: 'Reconnect' }), localized({ zh: '稍后', en: 'Later' })] })
          .then((answer) => { if (answer.response === 0) { app.relaunch(); quitAfterSave(true) } })
        return
      }
      void dialog.showMessageBox(window, { type: 'error', message: localized({ zh: '执行后端已停止', en: 'The execution backend stopped.' }),
        detail: localized({ zh: `${code === null ? '进程已被终止' : `退出代码：${code}`}。会话历史已保留。`,
          en: `${code === null ? 'The process was terminated' : `Exit code: ${code}`}. Your chat history is kept.` }) })
    },
  }
  /** Start the Host over SSH, choosing another port when the chosen one was taken here or on the server. */
  const startSshHost = async (selected: ExecutionTarget): Promise<HostReady> => {
    const destination = sshDestinationOf(selected)
    for (let attempt = 1; ; attempt++) {
      const session = await askpassSession()
      sshSaid.length = 0
      const candidate = new SshHostTransport({ ...hostOptions, target: destination, port: await freeLoopbackPort(),
        ...(session === undefined ? {} : { askpass: session.environment }) })
      transport = candidate
      try { return await candidate.start() }
      catch (error) {
        const said = sshSaid.join('\n')
        if (attempt < 3 && /forwarding|EADDRINUSE|address already in use/iu.test(`${said}\n${errorText(error)}`)) continue
        if (/Permission denied/u.test(said)) session?.failed()
        throw /The execution Host stopped/u.test(errorText(error)) ? sshFailure(destination, null, said) : error
      } finally { session?.dispose() }
    }
  }
  progress.step(localized({ zh: '正在启动执行后端…', en: 'Starting the execution backend…' }))
  let started: HostReady | undefined
  if (target.kind === 'ssh') {
    const selected = target
    started = await sshStep(selected, () => startSshHost(selected), false)
  } else {
    transport = target.kind === 'windows' ? new WindowsHostTransport({ ...hostOptions, cwd: installRoot }) : new WslHostTransport({ ...hostOptions, distro })
    started = await transport.start()
  }
  if (started === undefined) throw new Error('The execution Host did not start.')
  const ready = started
  hostReady = true
  timeline.mark('host')
  // Recording the resolved target lets the next launch skip the registry lookup and the environment window.
  if ((preferences.pendingProject !== undefined && preferences.pendingProject !== null) || !sameTarget(preferences.executionTarget, target))
    await saveExecutionTarget(settingsPath, target)
  const origin = new URL(ready.url).origin
  const trustedSender = (event: Pick<Electron.IpcMainEvent, 'sender' | 'senderFrame'>) => event.sender === window.webContents
    && event.senderFrame === window.webContents.mainFrame && new URL(event.senderFrame.url).origin === origin
  const ideEnvironment = target.kind === 'wsl' ? createIdeEnvironmentWindow({ parent: window, distro,
    resourceRoot: posix.resolve(posix.dirname(installed.host), '../resources/ide'),
    preloadPath, pagePath: join(__dirname, 'setup/ide.html'), icon,
  }) : undefined
  closeIdeEnvironment = () => ideEnvironment?.close() ?? Promise.resolve()
  const prepareComponents = async (): Promise<void> => {
    if (componentInstallation) return componentInstallation
    if (lifecycle.closing || lifecycle.switching) {
      throw new Error(localized({ zh: '应用正在退出或切换执行环境，请稍后再准备运行环境。',
        en: 'Rainy is quitting or switching its execution environment. Prepare the runtime environment later.' }))
    }
    const operation = async (): Promise<void> => {
      if (target.kind === 'ssh') {
        await dialog.showMessageBox(window, { title: 'Rainy', message: localized({ zh: '在服务器上安装工具', en: 'Install tools on the server' }),
          detail: localized({ zh: `${target.label} 上的解释器和工具请直接在服务器终端里安装（例如 apt、pip、nvm），然后在运行环境页点击“检测环境”。`,
            en: `Install interpreters and tools on ${target.label} from a terminal on the server (for example with apt, pip or nvm), then click "Detect existing environments" on the Runtime page.` }) })
        return
      }
      const importComponents = localized({ zh: '导入离线组件', en: 'Import offline components' })
      const cancel = localized({ zh: '取消', en: 'Cancel' })
      const choices = target.kind === 'wsl' ? [importComponents, localized({ zh: '准备 Ubuntu 开发工具', en: 'Prepare Ubuntu development tools' }), cancel]
        : [importComponents, localized({ zh: '准备 WSL 环境', en: 'Prepare a WSL environment' }), cancel]
      const action = await dialog.showMessageBox(window, { title: 'Rainy', message: localized({ zh: '准备运行环境', en: 'Prepare the runtime environment' }),
        detail: localized({ zh: '可直接复用已安装环境。导入只写入 Rainy 自有目录，不修改已有 Python、Conda 或系统 PATH。',
          en: 'Environments that are already installed can be used as they are. Importing writes only to folders that Rainy owns '
            + 'and does not change existing Python, Conda or the system PATH.' }), buttons: choices, cancelId: choices.length - 1 })
      if (action.response === choices.length - 1) return
      if (ideEnvironment !== undefined && action.response === 1) { await ideEnvironment.open(); return }
      if (target.kind === 'windows' && action.response === 1) {
        const prepared = await prepareDesktopEnvironment(environmentSetup)
        prepared.closeSetup()
        await dialog.showMessageBox(window, { title: 'Rainy', message: localized({ zh: 'WSL 环境已就绪', en: 'The WSL environment is ready.' }),
          detail: localized({ zh: '在运行环境页选择该 WSL 环境即可切换。', en: 'Select it on the Runtime page to switch to it.' }) })
        return
      }
      const selected = await dialog.showOpenDialog(window, { title: localized({ zh: '选择完整离线包中的组件 JSON', en: 'Select the component JSON in the full offline package' }),
        properties: ['openFile'], filters: [{ name: localized({ zh: '离线组件清单', en: 'Offline component list' }), extensions: ['json'] }] })
      const descriptorPath = selected.filePaths[0]
      if (selected.canceled || !descriptorPath) return
      if ((await stat(descriptorPath)).size > 65536) throw new Error(localized({ zh: '离线组件清单过大。', en: 'The offline component list is too large.' }))
      const supplied = environmentComponentSchema.parse(JSON.parse(await readFile(descriptorPath, 'utf8')))
      const catalog = await readEnvironmentComponentCatalog(join(resourceRoot, 'environment-component-catalog.json'))
      const expected = catalog.find(value => value.id === supplied.id && value.sha256 === supplied.sha256
        && value.manifestSha256 === supplied.manifestSha256)
      if (!expected || expected.platform !== (target.kind === 'windows' ? 'windows' : 'linux')) {
        throw new Error(localized({ zh: '该组件不属于当前发行包或执行环境。', en: 'This component does not belong to this Rainy version or execution environment.' }))
      }
      if (target.kind === 'windows') await installWindowsComponent({ mediaDirectory: dirname(descriptorPath), root: join(app.getPath('userData'), 'env'), component: expected,
        progress: (message) => { if (!window.isDestroyed()) window.webContents.send('rainy:component-progress', { message }) } })
      else {
        const installerPath = await convert(app.isPackaged ? join(resourceRoot, 'install-environment-component.py') : join(installRoot, 'scripts/install-environment-component.py'))
        const archive = await convert(join(dirname(descriptorPath), expected.file))
        await run('wsl.exe', ['-d', distro, '--exec', 'python3', installerPath, archive, JSON.stringify(expected), '--root', posix.join(ready.home, 'components')],
          { windowsHide: true, timeout: 30 * 60 * 1000, maxBuffer: 1024 * 1024 })
      }
      await dialog.showMessageBox(window, { title: 'Rainy', message: localized({ zh: '离线组件已校验并导入', en: 'The offline component was verified and imported.' }),
        detail: localized({ zh: '点击运行环境页的“检测环境”，然后选择需要的解释器。',
          en: 'On the Runtime page, click "Detect existing environments", then choose the interpreter you need.' }) })
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
      resolve({ ok: false, error: localized({ zh: '工具未能确认草稿已保存，请重试。', en: 'Rainy could not confirm that your drafts were saved. Try again.' }) })
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
      if (componentInstallation) {
        return { ok: false, error: localized({ zh: '请等待运行环境准备完成后再切换。', en: 'Wait for the runtime environment setup to finish before switching.' }) }
      }
      if (!transport) return { ok: false, error: localized({ zh: '应用正在退出或执行环境尚未就绪。', en: 'Rainy is quitting, or the execution environment is not ready yet.' }) }
      if ((await transport.inspectActivity()).active) {
        return { ok: false, error: localized({ zh: '请先结束运行中的 AI 任务、程序、调试及终端，再切换执行环境。',
          en: 'Finish running AI tasks, programs, debugging and terminals before switching the execution environment.' }) }
      }
      const saved = await flushWorkbench()
      if (!saved.ok) {
        return { ok: false, error: saved.error ?? localized({ zh: '草稿未保存，执行环境保持不变。', en: 'Your drafts were not saved. The execution environment is unchanged.' }) }
      }
      workspaceId ??= (await transport.inspectProject())?.workspaceId
      let pendingProject: PendingProjectTarget | undefined
      const projects = createProjectRegistry({ root: carrierState, targetId: target.id })
      // An SSH server keeps its own project records; a project does not move between it and this computer.
      if (workspaceId && target.kind !== 'ssh' && selected.kind !== 'ssh') {
        const snapshot = await transport.inspectProject(workspaceId)
        if (!snapshot) throw new Error(localized({ zh: '当前项目不可用，无法带到新的执行环境。', en: 'The selected project is unavailable for target migration.' }))
        const records = await projects.list()
        const project = records.find(item => item.projectId === snapshot.projectId)
        if (!project) {
          return { ok: false, error: localized({ zh: '当前项目尚未登记，请刷新运行环境页面后重试。',
            en: 'The current project is not registered yet. Refresh the Runtime page and try again.' }) }
        }
        const current = project.bindings.find(binding => binding.targetId === target.id && binding.workspaceId === workspaceId)
        if (!current) throw new Error(localized({ zh: '当前项目在这个执行环境中的记录不可用，请刷新运行环境页面后重试。', en: 'The current project binding is unavailable. Refresh the Runtime page and try again.' }))
        // Windows paths are the shared form between the current and the selected target.
        const mapToSelected = async (path: string): Promise<string> => {
          const windowsPath = target.kind === 'windows' ? path : await wslPath(distro, '-w', path)
          if (!win32.isAbsolute(windowsPath)) {
            throw new Error(localized({ zh: '项目路径无法映射到目标环境，请保留原环境。',
              en: 'The project path cannot be used in the selected environment. Keep the current environment.' }))
          }
          return selected.kind === 'windows' ? windowsPath : wslPath(requireWslDistribution(selected), '-u', windowsPath)
        }
        const path = project.bindings.find(binding => binding.targetId === selected.id)?.path || await mapToSelected(current.path)
        const roots = []
        for (const root of snapshot.roots.filter(value => !value.primary)) {
          roots.push({ rootId: root.rootId, path: await mapToSelected(root.path), title: root.title })
        }
        pendingProject = { projectId: project.projectId, path, roots }
      }
      if ((await transport.inspectActivity('freeze')).active) {
        return { ok: false, error: localized({ zh: '新的任务已开始，执行环境保持不变。', en: 'A new task has started. The execution environment is unchanged.' }) }
      }
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
      pendingFlush.resolve({ ok: false, error: localized({ zh: '窗口已关闭，无法确认草稿保存状态。',
        en: 'The window closed, so Rainy could not confirm whether your drafts were saved.' }) })
      pendingFlush = undefined
    }
    flushWorkbench = () => Promise.resolve({ ok: true })
  })
  onIpc('rainy:caption-colors', (event, value) => {
    if (!trustedSender(event)) return
    const colors = captionColors(value)
    if (!colors) return
    if (value !== null && typeof value === 'object' && 'locale' in value && (value.locale === 'zh' || value.locale === 'en')) {
      reportLocale(value.locale)
    }
    window.setTitleBarOverlay({ ...colors, color: '#00000000', height: CAPTION_HEIGHT })
    if (value !== null && typeof value === 'object' && 'theme' in value
      && (value.theme === 'dark' || value.theme === 'light' || value.theme === 'system')) nativeTheme.themeSource = value.theme
  })
  onIpc('rainy:select-distribution', (event) => { if (trustedSender(event)) requestDistribution() })
  // The preload asks once, synchronously: an SSH server has no Windows folder picker.
  onIpc('rainy:execution-kind', (event) => { event.returnValue = trustedSender(event) ? target.kind : '' })
  onIpc('rainy:native-menu', (event, value) => {
    if (!trustedSender(event) || value === null || typeof value !== 'object' || !('menu' in value) || !('locale' in value)
      || (value.menu !== 'edit' && value.menu !== 'help') || (value.locale !== 'zh' && value.locale !== 'en')) return
    reportLocale(value.locale)
    const template = nativeMenuTemplate(value.menu, value.locale, () => {
      void dialog.showMessageBox(window, { title: 'Rainy', message: `Rainy ${app.getVersion()}`, detail: 'Developed by RainyMarks' })
    }, () => { void updates?.check(true) })
    Menu.buildFromTemplate(template).popup({ window })
  })
  const selectDirectory = createIdeDirectoryPicker(async () => {
    if (target.kind === 'ssh') return null
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
  handleIpc('rainy:export-diagnostics', async (event) => {
    if (!trustedSender(event)) throw new Error(localized({ zh: '只能在 Rainy 窗口中导出诊断信息。', en: 'Diagnostics can be exported only from the Rainy window.' }))
    const stamp = new Date().toISOString().slice(0, 16).replace(/[-:T]/gu, '')
    const chosen = await dialog.showSaveDialog(window, {
      title: localized({ zh: '导出诊断信息', en: 'Export diagnostics' }),
      defaultPath: join(app.getPath('downloads'), `Rainy-diagnostics-${stamp}.zip`),
      filters: [{ name: 'Zip', extensions: ['zip'] }],
    })
    if (chosen.canceled || !chosen.filePath) return null
    const crashes = app.getPath('crashDumps')
    const dumps = await newestFiles(['reports', 'completed', 'pending', 'new'].map(folder => join(crashes, folder)), /\.dmp$/iu, 5)
    const strataLogs = await newestFiles([join(userData, 'strata', 'logs')], /\.log$/iu, 3)
    await exportDiagnostics(chosen.filePath, {
      Rainy: app.getVersion(), Installed: String(app.isPackaged),
      Electron: process.versions.electron, Chromium: process.versions.chrome, Node: process.versions.node,
      Windows: `${version()} (${release()}, ${arch()})`, 'Execution target': target.label, 'Interface language': currentLocale(),
      Memory: `${Math.round(totalmem() / 1024 ** 3)} GiB, ${Math.round(freemem() / 1024 ** 3)} GiB free`, Exported: new Date().toISOString(),
    }, [
      { name: 'logs/host.log', path: logPath, maxBytes: 4 * 1024 ** 2, text: true },
      { name: 'desktop.json', path: settingsPath, maxBytes: 64 * 1024, text: true },
      ...strataLogs.map(path => ({ name: `logs/strata/${basename(path)}`, path, maxBytes: 1024 ** 2, text: true })),
      ...dumps.map(path => ({ name: `crashes/${basename(path)}`, path, maxBytes: 64 * 1024 ** 2, text: false })),
    ])
    shell.showItemInFolder(chosen.filePath)
    return chosen.filePath
  })
  handleIpc('rainy:ide-directory', (event) => {
    if (!trustedSender(event)) throw new Error('Directory selection is available only in the Rainy window')
    return selectDirectory()
  })
  const officeConverter = new OfficeConverter()
  handleIpc('rainy:office-pdf', async (event, value) => {
    if (!trustedSender(event) || !isRecord(value) || !(value.bytes instanceof Uint8Array) || typeof value.extension !== 'string') {
      throw new Error('Invalid document conversion request.')
    }
    // Failures are returned rather than thrown, so the page shows the reason without Electron's IPC prefix.
    try {
      const result = await officeConverter.toPdf(value.bytes, value.extension.toLowerCase())
      return { pdf: new Uint8Array(result.pdf), application: result.application }
    } catch (error) {
      return { error: error instanceof Error ? error.message : String(error), missing: error instanceof OfficeConversionError && error.missing,
        encrypted: error instanceof OfficeConversionError && error.encrypted }
    }
  })
  handleIpc('rainy:open-file', async (event, value) => {
    if (!trustedSender(event) || !isRecord(value) || typeof value.path !== 'string' || value.path === '') throw new Error('Invalid open request.')
    const extension = value.path.slice(value.path.lastIndexOf('.') + 1).toLowerCase()
    // Only documents: a shortcut, script or program would run instead of being shown.
    if (officeKind(extension) === undefined && !OPENABLE_DOCUMENTS.has(extension)) return { error: localized({ zh: '只能用系统程序打开文档。', en: 'Only documents can be opened in another app.' }) }
    if (target.kind === 'ssh') return { error: localized({ zh: '文件在 SSH 服务器上，无法用本机程序打开。', en: 'The file is on the SSH server; apps on this computer cannot open it.' }) }
    const path = target.kind === 'wsl' ? await wslPath(distro, '-w', value.path) : value.path
    const failure = await shell.openPath(path)
    return failure === '' ? {} : { error: failure }
  })
  handleIpc('rainy:ide-prepare-development', async (event) => {
    if (!trustedSender(event)) throw new Error('Development setup is available only in the Rainy window')
    await prepareComponents()
  })
  handleIpc('rainy:runtime-targets', async (event) => {
    if (!trustedSender(event)) throw new Error('Runtime selection is available only in the Rainy window.')
    return { current: target, targets: await availableTargets() }
  })
  handleIpc('rainy:runtime-ssh-add', async (event, value) => {
    if (!trustedSender(event) || !isRecord(value) || typeof value.destination !== 'string' || (value.port !== undefined && typeof value.port !== 'number')) {
      throw new Error('Invalid SSH host request.')
    }
    const destination = parseSshDestination(value.destination, value.port as number | undefined)
    // One connection checks the address, the login, the system and python3 before the host is offered.
    const session = await askpassSession()
    try {
      const result = await runSsh(destination, remotePython('import platform\nprint(platform.system(), platform.machine())\n'),
        { ...(session === undefined ? {} : { environment: session.environment }), timeoutMs: 180000 })
      if (result.code !== 0) {
        if (/Permission denied/u.test(result.stderr)) session?.failed()
        throw sshFailure(destination, result.code, result.stderr)
      }
      const [system, machine] = result.stdout.trim().split(/\s+/u)
      if (system !== 'Linux' || (machine !== 'x86_64' && machine !== 'amd64')) {
        throw new Error(localized({ zh: `${sshName(destination)} 是 ${system ?? '?'} ${machine ?? '?'}；SSH 远程开发目前只支持 x86_64 Linux 服务器。`,
          en: `${sshName(destination)} runs ${system ?? '?'} ${machine ?? '?'}; SSH remote development supports x86_64 Linux servers only.` }))
      }
    } finally { session?.dispose() }
    await saveSshHost(settingsPath, destination)
    return { current: target, targets: await availableTargets(), added: sshTarget(destination).id }
  })
  handleIpc('rainy:runtime-ssh-remove', async (event, value) => {
    if (!trustedSender(event) || !isRecord(value) || typeof value.targetId !== 'string') throw new Error('Invalid SSH host request.')
    if (value.targetId === target.id) throw new Error(localized({ zh: '不能移除正在使用的 SSH 主机。', en: 'The SSH host in use cannot be removed.' }))
    await removeSshHost(settingsPath, value.targetId)
    return { current: target, targets: await availableTargets() }
  })
  handleIpc('rainy:runtime-switch', async (event, value) => {
    if (!trustedSender(event) || value === null || typeof value !== 'object' || !('targetId' in value) || typeof value.targetId !== 'string') throw new Error('Invalid execution target request.')
    const selected = await findTarget(value.targetId)
    if (!selected) throw new Error(localized({ zh: '所选执行环境已不可用，例如 WSL 发行版或 SSH 主机已被移除。', en: 'The selected execution target is unavailable, for example because its WSL distribution or SSH host was removed.' }))
    return switchTarget(selected, 'workspaceId' in value && typeof value.workspaceId === 'string' ? brandString<WorkspaceId>(value.workspaceId) : undefined)
  })
  const admitStrata = (event: Electron.IpcMainInvokeEvent, mutate = false): void => {
    if (!trustedSender(event)) throw new Error('Strata controls are available only in the Rainy window.')
    if (mutate && (lifecycle.closing || lifecycle.switching)) {
      throw new Error(localized({ zh: '请等待应用切换或退出完成后再操作 Strata。', en: 'Wait for Rainy to finish switching or quitting before using Strata.' }))
    }
  }
  const admitModule = (event: Electron.IpcMainInvokeEvent): void => {
    if (!trustedSender(event)) throw new Error('Component downloads are available only in the Rainy window.')
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
    if (target.kind === 'ssh') {
      throw new Error(localized({ zh: 'SSH 服务器上的后端访问不到这台电脑上的 Strata 本地模型；请在服务器能访问的地址上配置模型。',
        en: 'The backend on the SSH server cannot reach the Strata local model on this computer. Configure a model at an address the server can reach.' }))
    }
    const connection = await strata.connection()
    const statusResponse = await window.webContents.session.fetch(controlURL, { credentials: 'include' })
    const status = await controlBody(statusResponse)
    if (!statusResponse.ok || status === null || typeof status !== 'object' || !('models' in status) || !Array.isArray(status.models)) {
      throw new Error(localized({ zh: '当前执行环境的模型设置不可用，请稍后重试。', en: 'The model settings of the current execution environment are unavailable. Try again later.' }))
    }
    const previous: unknown = status.models.find((value: unknown) => value !== null && typeof value === 'object'
      && 'provider' in value && value.provider === 'rainy-strata')
    let approvedBudget: { maxTokens: number; expectedContextWindow: number } | undefined
    if (previous !== null && typeof previous === 'object' && 'maxTokens' in previous && typeof previous.maxTokens === 'number') {
      try { resolveBudget(connection.contextWindow, previous.maxTokens) }
      catch (_incompatibleBudget) {
        const maxTokens = resolveBudget(connection.contextWindow).outputTokens
        const answer = await dialog.showMessageBox(window, currentLocale() === 'zh'
          ? { type: 'question', title: 'Strata 输出上限', message: `当前上下文为 ${connection.contextWindow}，原输出上限 ${previous.maxTokens} 过大。`,
            detail: `将输出上限调整为 ${maxTokens} 并连接？推理档位保持不变。`, buttons: ['取消', '调整并连接'], defaultId: 0, cancelId: 0 }
          : { type: 'question', title: 'Strata output limit', message: `The ${previous.maxTokens} output limit does not fit the ${connection.contextWindow} context.`,
            detail: `Set the output limit to ${maxTokens} and connect? The thinking setting stays unchanged.`, buttons: ['Cancel', 'Adjust and connect'], defaultId: 0, cancelId: 0 })
        if (answer.response !== 1) throw new Error(currentLocale() === 'zh' ? '已取消连接，原模型设置保持不变。' : 'Connection cancelled. Model settings are unchanged.')
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
      || typeof value.result.provider !== 'string' || typeof value.result.model !== 'string') {
      throw new Error(localized({ zh: 'Strata 模型配置未完成，请重新检查当前执行环境。', en: 'The Strata model setup did not finish. Check the current execution environment again.' }))
    }
    return { provider: value.result.provider, model: value.result.model }
  })
  writeFileSync(join(app.getPath('userData'), 'host.json'), JSON.stringify({ target, distro, pid: ready.pid, home: ready.home, origin, runtime: installed.host }, null, 2) + '\n')
  const mappedPaths = new Map<string, string>()
  // Dropped-file paths are resolved synchronously because the renderer bridge returns them inline.
  onIpc('rainy:path', (event, path) => {
    event.returnValue = ''
    if (!trustedSender(event) || typeof path !== 'string' || !win32.isAbsolute(path) || path.includes('\0')) return
    if (target.kind === 'windows') { event.returnValue = path; return }
    // Files on this computer are not on the SSH server.
    if (target.kind === 'ssh') return
    try {
      const mapped = mappedPaths.get(path) ?? execFileSync('wsl.exe', ['-d', distro, '--exec', 'wslpath', '-u', path], { windowsHide: true, encoding: 'utf8', timeout: 5000 }).trim()
      if (!posix.isAbsolute(mapped)) return
      mappedPaths.set(path, mapped)
      event.returnValue = mapped
    } catch (error) { appendFileSync(logPath, `A dropped file could not be mapped into WSL: ${errorText(error)}\n`) }
  })
  window.webContents.setWindowOpenHandler(({ url }) => { if (/^https?:/.test(url)) void shell.openExternal(url); return { action: 'deny' } })
  // The page never navigates to project file bytes either: a link in a previewed document must not replace the workbench.
  window.webContents.on('will-navigate', (event, url) => {
    const target = new URL(url)
    if (target.origin !== origin || target.pathname === IDE_RAW_ROUTE) event.preventDefault()
  })
  window.webContents.session.setPermissionRequestHandler((_contents, permission, callback, details) => {
    const url = details.requestingUrl
    callback(allowsClipboardWrite(permission, url, origin))
  })
  window.webContents.on('page-title-updated', (event) => { event.preventDefault(); window.setTitle('Rainy') })
  progress.step(localized({ zh: '正在加载工作台…', en: 'Loading the workbench…' }))
  await window.loadURL(ready.url)
  timeline.mark('workbench')
  appendFileSync(logPath, `Rainy startup (${target.label}): ${timeline.summary()}\n`)
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
      void pruneWindowsHost(resourceRoot).then((bytes) => {
        if (bytes > 0) appendFileSync(logPath, `Removed ${Math.round(bytes / 1024 ** 2)} MiB of Windows runtime files that earlier versions left behind\n`)
      }, (error: unknown) => { appendFileSync(logPath, `Old Windows runtime files were not removed: ${errorText(error)}\n`) })
    }
    if (app.isPackaged && target.kind === 'ssh') {
      void sshInstaller(target, ['--prune']).then((result) => {
        if (isRecord(result) && Array.isArray(result.removed) && result.removed.length) appendFileSync(logPath, `Removed ${result.removed.length} Linux runtimes that earlier versions installed on ${target.label}\n`)
      }, (error: unknown) => { appendFileSync(logPath, `Earlier Linux runtimes on ${target.label} were not removed: ${errorText(error)}\n`) })
    }
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
      await dialog.showMessageBox(options.window, { type: 'warning', title: 'Rainy',
        message: localized({ zh: 'Rainy 安装文件已被修改或损坏', en: 'Rainy installation files were changed or damaged.' }),
        detail: localized({ zh: `${error.message}\n下次启动时会重新完整校验；如果问题仍然存在，请重新安装 Rainy。`,
          en: `${error.message}\nAll files will be checked again at the next start. If the problem remains, reinstall Rainy.` }) })
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

// The uninstaller removes the bundled Windows runtime through the app, file by file from its inventory, instead of deleting a folder tree.
if (process.argv.includes('--rainy-remove-runtime')) {
  void app.whenReady().then(async () => { if (app.isPackaged) await pruneWindowsHost(process.resourcesPath, { everything: true }); app.exit(0) })
    .catch((error: unknown) => { console.error('Removing the Windows runtime failed', error); app.exit(1) })
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
    await dialog.showMessageBox({ type: 'error', message: localized({ zh: 'Rainy 启动失败', en: 'Rainy could not start.' }), detail: errorText(error) })
    app.quit()
  })
}
