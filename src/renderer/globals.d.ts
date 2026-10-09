/** Values the Host injects into `index.html` and the bridges the Electron preload exposes. */
import type { Config } from '../shared/config.ts'

declare global {
  interface Window {
    __RAINY_AGENT__?: { name: 'RainyAgent'; version: string; environment: 'Windows' | 'WSL' }
    __RAINY_WORKBENCH_CONFIG__?: Config
  }
}
