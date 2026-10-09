/** Values the Host injects into `index.html` and the bridges the Electron preload exposes. */
import type { Config } from '../shared/config.ts'

declare global {
  interface Window {
    __RAINY_AGENT__?: { name: 'RainyAgent'; version: string; environment: 'Windows' | 'WSL' }
    __RAINY_WORKBENCH_CONFIG__?: Config
  }
}

declare global {
  interface Window {
    /** Native folder picker; absent outside the desktop app. */
    __RAINY_IDE_NATIVE__?: import('./app/workbench.ts').NativeIdeBridge
    /** Native CTF tool catalog; absent outside the desktop app. */
    __RAINY_TOOLS__?: import('../shared/native-tools-protocol.ts').NativeToolsBridge
    /** Save hooks the desktop runs before it closes or switches the Host; absent outside the desktop app. */
    __RAINY_WORKBENCH__?: import('./ctf/ctf-protocol.ts').DesktopWorkbenchBridge
    /** Execution-target path of a dropped desktop file (`''` when it has none); absent outside the desktop app. */
    __RAINY_HOST_PATHS__?: { pathFor(file: File): string }
  }
}
