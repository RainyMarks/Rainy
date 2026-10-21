/** Values the Host injects into `index.html` and the bridges the Electron preload exposes. */
import type { Config } from '../shared/config.ts'

declare global {
  interface Window {
    __RAINY_AGENT__?: { name: 'Rainy'; version: string; environment: 'Windows' | 'WSL' | 'SSH'; label?: string }
    __RAINY_WORKBENCH_CONFIG__?: Config
  }
}

declare global {
  interface Window {
    /** Native folder picker; absent outside the desktop app. */
    __RAINY_IDE_NATIVE__?: import('./app/workbench.ts').NativeIdeBridge
    /** Save hooks the desktop runs before it closes or switches the Host; absent outside the desktop app. */
    __RAINY_WORKBENCH__?: import('./app/desktop-bridge.ts').DesktopWorkbenchBridge
    /** Execution-target path of a dropped desktop file (`''` when it has none); absent outside the desktop app. */
    __RAINY_HOST_PATHS__?: { pathFor(file: File): string }
    /** Office conversion and "open in default app"; absent outside the desktop app. */
    __RAINY_OFFICE__?: import('../shared/office-protocol.ts').OfficeBridge
    /** Saves an exported document through the native save dialog, printing it to PDF first; absent outside the desktop app. */
    __RAINY_EXPORT__?: {
      save(request: { format: 'pdf' | 'html'; html: string; name: string; directory?: string | undefined }): Promise<{ path: string } | null>
      /** Show a saved file selected in Explorer. */
      showInFolder(path: string): Promise<void>
    }
  }
}
