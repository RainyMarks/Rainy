/** The CTF workbench's long-lived parts: IceSky frame bridge, native tool catalog and the selected view. */
import type { Config } from '../../shared/config.ts'
import type { NativeToolsBridge } from '../../shared/native-tools-protocol.ts'
import { createStore, toast, type Store } from '../ui/index.ts'
import { CtfWorkbenchBridge } from './ctf-bridge.ts'
import { readCtfColors } from './ctf-colors.ts'
import type { CtfConfiguration, DesktopWorkbenchBridge } from './ctf-protocol.ts'
import { NativeToolsController } from './native-tools.ts'
import { ctfMessages } from './messages.ts'

/** Tab shown in the workbench. */
export type CtfView = 'catalog' | 'icesky'

/** Owner of the frame bridge and tool catalog; outlives the workbench tab. */
export interface CtfWorkbench {
  readonly bridge: CtfWorkbenchBridge
  readonly tools: NativeToolsController
  readonly view: Store<CtfView>
  /**
   * Publish the chat context, appearance and visibility to the IceSky frame.
   * @param input Current chat (`null` for drafts outside any chat), appearance and whether the IceSky tab is on screen.
   */
  configure(input: { sessionId: string | null; appearance: Omit<CtfConfiguration['appearance'], 'colors'>; visible: boolean }): void
  /** Stop listening to the frame and the desktop bridges. */
  dispose(): void
}

/**
 * Create the workbench owner and register its desktop save hook.
 * @param config Host-resolved deadlines.
 * @param bridges Desktop bridges; absent in a plain browser.
 * @returns The owner.
 */
export function createCtfWorkbench(config: Pick<Config, 'readyTimeoutMs' | 'flushTimeoutMs'>, bridges: {
  readonly tools?: NativeToolsBridge | undefined
  readonly desktop?: DesktopWorkbenchBridge | undefined
} = {}): CtfWorkbench {
  const { t } = ctfMessages
  const notify = (message: string, kind: 'success' | 'error' | 'warning'): void => {
    toast(message, kind === 'success' ? { tone: 'success' } : {})
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
  const receive = (event: MessageEvent<unknown>): void => { bridge.receive(event) }
  window.addEventListener('message', receive)
  const unregister = bridges.desktop?.onFlush(() => bridge.flush())
  return {
    bridge,
    tools,
    view,
    configure: ({ sessionId, appearance, visible }) => {
      const colors = readCtfColors(document)
      bridge.configure({
        context: sessionId === null ? { kind: 'standalone' } : { kind: 'session', id: sessionId },
        appearance: { ...appearance, ...colors === undefined ? {} : { colors } },
        visible,
      })
    },
    dispose: () => {
      unregister?.()
      window.removeEventListener('message', receive)
      bridge.dispose()
      tools.dispose()
    },
  }
}
