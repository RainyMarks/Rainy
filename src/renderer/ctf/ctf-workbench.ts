/** The CTF workbench's long-lived parts: IceSky frame bridge, native tool catalog and the selected view. */
import type { Config } from '../../shared/config.ts'
import type { NativeToolsBridge } from '../../shared/native-tools-protocol.ts'
import { createStore, toast, type Store } from '../ui/index.ts'
import { CtfWorkbenchBridge } from './ctf-bridge.ts'
import { readCtfColors, readCtfFonts } from './ctf-colors.ts'
import type { CtfConfiguration, DesktopWorkbenchBridge } from './ctf-protocol.ts'
import { NativeToolsController } from './native-tools.ts'
import { ctfMessages } from './messages.ts'

/** Tab shown in the workbench. */
export type CtfView = 'catalog' | 'icesky'

/** The IceSky page is unloaded after it has been out of sight this long, if everything on it is saved. */
const UNLOAD_AFTER_MS = 10 * 60_000

/** Owner of the frame bridge and tool catalog; outlives the workbench tab. */
export interface CtfWorkbench {
  readonly bridge: CtfWorkbenchBridge
  readonly tools: NativeToolsController
  readonly view: Store<CtfView>
  /** Whether the IceSky page is loaded: from when it is first shown until it is unloaded out of sight. */
  readonly frameLoaded: Store<boolean>
  /**
   * Publish the chat context, appearance and visibility to the IceSky frame.
   * @param input Current chat (`null` for drafts outside any chat), appearance and whether the IceSky tab is on screen.
   */
  configure(input: { sessionId: string | null; appearance: Omit<CtfConfiguration['appearance'], 'colors' | 'fonts'>; visible: boolean }): void
  /** Stop listening to the frame and the desktop bridges. */
  dispose(): void
}

/**
 * Create the workbench owner and register its desktop save hook.
 * @param config Host-resolved deadlines; `unloadAfterMs` overrides the out-of-sight time before the IceSky page unloads.
 * @param bridges Desktop bridges; absent in a plain browser.
 * @returns The owner.
 */
export function createCtfWorkbench(config: Pick<Config, 'readyTimeoutMs' | 'flushTimeoutMs'> & { readonly unloadAfterMs?: number }, bridges: {
  readonly tools?: NativeToolsBridge | undefined
  readonly desktop?: DesktopWorkbenchBridge | undefined
} = {}): CtfWorkbench {
  const { t } = ctfMessages
  const notify = (message: string, kind: 'success' | 'error' | 'warning'): void => {
    toast(message, kind === 'warning' ? {} : { tone: kind })
  }
  const bridge = new CtfWorkbenchBridge({ origin: location.origin, readyTimeoutMs: config.readyTimeoutMs,
    flushTimeoutMs: config.flushTimeoutMs, toast: notify, flushFailureMessage: () => t('ctfSaveFailed') })
  const tools = new NativeToolsController(bridges.tools, {
    opened: name => t('toolsOpened', { name }),
    launchFailed: name => t('toolsLaunchFailed', { name }),
    favoritesFailed: () => t('toolsFavoritesFailed'),
    favoriteSaved: selected => t(selected ? 'toolsFavoriteSaved' : 'toolsFavoriteRemoved'),
    completed: operation => t(({ install: 'toolsDoneInstall', update: 'toolsDoneUpdate', remove: 'toolsDoneRemove', repair: 'toolsDoneRepair' } as const)[operation]),
    downloadFailed: () => t('toolsDownloadFailed'),
  }, notify)
  const view = createStore<CtfView>('catalog')
  const frameLoaded = createStore(false)
  const receive = (event: MessageEvent<unknown>): void => { bridge.receive(event) }
  window.addEventListener('message', receive)
  const unregister = bridges.desktop?.onFlush(() => bridge.flush())
  // Out of sight, IceSky has already stopped its work. Its memory is released only after a confirmed save, and only when
  // a reload restores everything: selected local files and generated files are not part of saved drafts.
  let shown = false
  let shows = 0
  let disposed = false
  let unloadTimer: ReturnType<typeof setTimeout> | undefined
  const unloadLater = (): void => {
    clearTimeout(unloadTimer)
    unloadTimer = setTimeout(() => { unloadTimer = undefined; void unload() }, config.unloadAfterMs ?? UNLOAD_AFTER_MS)
  }
  const unload = async (): Promise<void> => {
    const before = shows
    const saved = await bridge.flush()
    if (disposed || shown) return
    if (shows === before && saved.ok && saved.restorable === true) {
      bridge.unloaded()
      frameLoaded.set(false)
    } else unloadLater()
  }
  return {
    bridge,
    tools,
    view,
    frameLoaded,
    configure: ({ sessionId, appearance, visible }) => {
      const colors = readCtfColors(document)
      const fonts = readCtfFonts(document)
      bridge.configure({
        context: sessionId === null ? { kind: 'standalone' } : { kind: 'session', id: sessionId },
        appearance: { ...appearance, ...colors === undefined ? {} : { colors }, ...fonts === undefined ? {} : { fonts } },
        visible,
      })
      if (visible) {
        if (!shown) shows++
        shown = true
        clearTimeout(unloadTimer)
        unloadTimer = undefined
        frameLoaded.set(true)
      } else if (shown) {
        shown = false
        unloadLater()
      }
    },
    dispose: () => {
      disposed = true
      clearTimeout(unloadTimer)
      unregister?.()
      window.removeEventListener('message', receive)
      bridge.dispose()
      tools.dispose()
    },
  }
}
