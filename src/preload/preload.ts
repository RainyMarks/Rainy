/** Adapt native file references to the selected WSL Host without exposing Electron APIs. */
import { contextBridge, ipcRenderer, webUtils } from 'electron'
import { installWindowChrome } from './preload-chrome.ts'
import type { EnvironmentAction, EnvironmentSnapshot } from './environment.ts'
import type { NativeToolCatalog, NativeToolId, NativeToolLaunchResult, NativeToolsDownloadState, NativeToolsUpdateState } from '@deepseek-ai/dsh-client-ui-rainy/native-tools-protocol'
import type { ToolPackWindowProgress } from './toolpack-maintenance.ts'
import type { IdeNativeDirectory } from './ide-native.ts'
import type { IdeEnvironmentAction, IdeEnvironmentSnapshot } from './ide-environment.ts'
import type { StrataNativeHost, StrataModelPicker, StrataSettings } from '@deepseek-ai/dsh-client-ui-rainy/strata-protocol'
import type { OptionalModuleId, OptionalModulesBridge, OptionalModulesState } from '@deepseek-ai/dsh-client-ui-rainy/modules-protocol'

/** Payload sent by the main process on each progress channel. */
interface ProgressChannels {
  'rainy:toolpack-progress': ToolPackWindowProgress
  'rainy:environment-progress': EnvironmentSnapshot
  'rainy:ide-environment-progress': IdeEnvironmentSnapshot
  'rainy:tools-download-progress': NativeToolsDownloadState
  'rainy:modules-progress': OptionalModulesState
  /** Validated by the receiving bridge, which forwards only the message text. */
  'rainy:component-progress': unknown
}

/** Forward one main-process event channel to a page listener. @returns the unsubscribe callback. */
function subscribe<C extends keyof ProgressChannels>(channel: C, listener: (value: ProgressChannels[C]) => void): () => void {
  const receive = (_event: Electron.IpcRendererEvent, value: ProgressChannels[C]): void => { listener(value) }
  ipcRenderer.on(channel, receive)
  return () => { ipcRenderer.removeListener(channel, receive) }
}

/** Setup pages receive a bridge only when this main frame is the page named by the opening window. */
function isSetupPage(argumentName: string): boolean {
  const prefix = `--${argumentName}=`
  const argument = process.argv.find(value => value.startsWith(prefix))
  return process.isMainFrame && argument !== undefined && location.href === decodeURIComponent(argument.slice(prefix.length))
}

if (isSetupPage('rainy-toolpack-page')) {
  contextBridge.exposeInMainWorld('__RAINY_TOOLPACK__', {
    getProgress: (): Promise<ToolPackWindowProgress> => ipcRenderer.invoke('rainy:toolpack-status'),
    cancel: (): Promise<void> => ipcRenderer.invoke('rainy:toolpack-cancel'),
    onProgress: (listener: (snapshot: ToolPackWindowProgress) => void) => subscribe('rainy:toolpack-progress', listener),
  })
}

if (isSetupPage('rainy-setup-page')) {
  contextBridge.exposeInMainWorld('__RAINY_ENVIRONMENT__', {
    inspect: (): Promise<EnvironmentSnapshot> => ipcRenderer.invoke('rainy:environment-inspect'),
    act: (action: EnvironmentAction): Promise<EnvironmentSnapshot> => ipcRenderer.invoke('rainy:environment-act', action),
    onProgress: (listener: (snapshot: EnvironmentSnapshot) => void) => subscribe('rainy:environment-progress', listener),
  })
}

if (isSetupPage('rainy-ide-environment-page')) {
  contextBridge.exposeInMainWorld('__RAINY_IDE_ENVIRONMENT__', {
    inspect: (): Promise<IdeEnvironmentSnapshot> => ipcRenderer.invoke('rainy:ide-environment-inspect'),
    act: (action: IdeEnvironmentAction): Promise<IdeEnvironmentSnapshot> => ipcRenderer.invoke('rainy:ide-environment-act', action),
    close: (): Promise<void> => ipcRenderer.invoke('rainy:ide-environment-close'),
    onProgress: (listener: (snapshot: IdeEnvironmentSnapshot) => void) => subscribe('rainy:ide-environment-progress', listener),
  })
}

if (process.isMainFrame && location.protocol === 'http:' && location.hostname === '127.0.0.1') {
  if (process.platform === 'win32') installWindowChrome()
  contextBridge.exposeInMainWorld('__DSH_HOST_PATHS__', {
    pathFor(file: File): string {
      const path = webUtils.getPathForFile(file)
      return path ? String(ipcRenderer.sendSync('rainy:path', path) ?? '') : ''
    },
  })
  contextBridge.exposeInMainWorld('__RAINY_IDE_NATIVE__', {
    selectDirectory: (): Promise<IdeNativeDirectory | null> => ipcRenderer.invoke('rainy:ide-directory'),
    prepareDevelopmentTools: (): Promise<void> => ipcRenderer.invoke('rainy:ide-prepare-development'),
  })
  contextBridge.exposeInMainWorld('__RAINY_STRATA_NATIVE__', {
    status: () => ipcRenderer.invoke('rainy:strata-status'),
    save: (settings: StrataSettings) => ipcRenderer.invoke('rainy:strata-save', settings),
    start: () => ipcRenderer.invoke('rainy:strata-start'),
    stop: () => ipcRenderer.invoke('rainy:strata-stop'),
    selectModel: (kind: StrataModelPicker) => ipcRenderer.invoke('rainy:strata-select-model', kind),
    connect: () => ipcRenderer.invoke('rainy:strata-connect'),
  } satisfies StrataNativeHost)
  contextBridge.exposeInMainWorld('__RAINY_RUNTIME_NATIVE__', {
    targets: () => ipcRenderer.invoke('rainy:runtime-targets'),
    switchTarget: (request: { targetId: string; workspaceId?: string }) => ipcRenderer.invoke('rainy:runtime-switch', request),
    prepare: (): Promise<void> => ipcRenderer.invoke('rainy:ide-prepare-development'),
    onProgress: (listener: (message: string) => void) => subscribe('rainy:component-progress', (value) => {
      if (value !== null && typeof value === 'object' && 'message' in value && typeof value.message === 'string') listener(value.message)
    }),
  })
  const modules: OptionalModulesBridge = {
    list: () => ipcRenderer.invoke('rainy:modules-list'),
    state: () => ipcRenderer.invoke('rainy:modules-state'),
    install: (id: OptionalModuleId) => ipcRenderer.invoke('rainy:modules-install', id),
    remove: (id: OptionalModuleId) => ipcRenderer.invoke('rainy:modules-remove', id),
    cancel: () => ipcRenderer.invoke('rainy:modules-cancel'),
    onProgress: receive => subscribe('rainy:modules-progress', receive),
  }
  contextBridge.exposeInMainWorld('__RAINY_MODULES__', modules)
  contextBridge.exposeInMainWorld('__RAINY_TOOLS__', {
    checkToolUpdates: (): Promise<NativeToolsUpdateState> => ipcRenderer.invoke('rainy:tools-check-updates'),
    getDownloadState: (): Promise<NativeToolsDownloadState> => ipcRenderer.invoke('rainy:tools-download-state'),
    installTools: (ids: readonly NativeToolId[]): Promise<void> => ipcRenderer.invoke('rainy:tools-install', ids),
    removeTool: (id: NativeToolId): Promise<void> => ipcRenderer.invoke('rainy:tools-remove', id),
    repairTools: (): Promise<void> => ipcRenderer.invoke('rainy:tools-repair'),
    cancelDownload: (): Promise<void> => ipcRenderer.invoke('rainy:tools-download-cancel'),
    onDownloadProgress: (receive: (state: NativeToolsDownloadState) => void) => subscribe('rainy:tools-download-progress', receive),
    listTools: (): Promise<NativeToolCatalog> => ipcRenderer.invoke('rainy:tools-list'),
    launchTool: (id: NativeToolId, variant?: 'x32'): Promise<NativeToolLaunchResult> => ipcRenderer.invoke('rainy:tools-launch', { id, ...variant === undefined ? {} : { variant } }),
    setFavorites: (ids: readonly NativeToolId[]): Promise<void> => ipcRenderer.invoke('rainy:tools-favorites', ids),
  })
  const saves = new Set<() => Promise<{ readonly ok: boolean; readonly error?: string }>>()
  contextBridge.exposeInMainWorld('__RAINY_WORKBENCH__', {
    onFlush(flush: () => Promise<{ readonly ok: boolean; readonly error?: string }>): () => void {
      saves.add(flush)
      return () => { saves.delete(flush) }
    },
  })
  const flush = async (_event: Electron.IpcRendererEvent, id: unknown): Promise<void> => {
    if (typeof id !== 'string') return
    try {
      const results = await Promise.all([...saves].map(save => save()))
      const failure = results.find(result => !result.ok)
      ipcRenderer.send('rainy:workbench-flushed', { id, ...(failure ?? { ok: true }) })
    } catch (error) {
      ipcRenderer.send('rainy:workbench-flushed', { id, ok: false, error: error instanceof Error ? error.message : String(error) })
    }
  }
  const handleFlush = (event: Electron.IpcRendererEvent, id: unknown): void => { void flush(event, id) }
  ipcRenderer.on('rainy:flush-workbench', handleFlush)
  window.addEventListener('unload', () => { ipcRenderer.removeListener('rainy:flush-workbench', handleFlush); saves.clear() }, { once: true })
}
