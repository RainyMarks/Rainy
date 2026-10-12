/** Electron preload bridges used by Settings; each one is absent when the page runs in a plain browser. */
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { OptionalModulesBridge } from '../../shared/modules-protocol.ts'
import type { StrataNativeHost } from '../../shared/strata-protocol.ts'

/** A Windows installation or WSL distribution the desktop can run the Host in. */
export interface RuntimeNativeTarget { id: string; kind: 'windows' | 'wsl'; label: string; distro?: string | undefined }

/** `window.__RAINY_RUNTIME_NATIVE__`: execution-target switching and offline component preparation. */
export interface RuntimeNativeHost {
  /** @returns The target the Host runs in and every available target. */
  targets(): Promise<{ current: RuntimeNativeTarget; targets: RuntimeNativeTarget[] }>
  /**
   * Restart the Host in another target, carrying the open project over.
   * @param request Target id and the current project.
   * @returns `ok: false` with a reason when the desktop refused the switch.
   */
  switchTarget(request: { targetId: string; workspaceId?: WorkspaceId | undefined }): Promise<{ ok: boolean; error?: string | undefined }>
  /** @returns After the offline development components are prepared. */
  prepare(): Promise<void>
  /**
   * @param listener Receives each preparation progress message.
   * @returns A function that removes the listener.
   */
  onProgress(listener: (message: string) => void): () => void
}

/** `window.__RAINY_DIAGNOSTICS__`: the local diagnostics export. */
export interface DiagnosticsNativeHost {
  /** @returns The saved zip, shown in its folder, or `null` when the user cancelled the save dialog. */
  export(): Promise<string | null>
}

interface Bridges {
  __RAINY_DIAGNOSTICS__?: DiagnosticsNativeHost
  __RAINY_STRATA_NATIVE__?: StrataNativeHost
  __RAINY_RUNTIME_NATIVE__?: RuntimeNativeHost
  __RAINY_MODULES__?: OptionalModulesBridge
}

const bridges = (): Bridges => globalThis as typeof globalThis & Bridges

/** @returns The Strata bridge, or `undefined` outside the desktop app. */
export function strataBridge(): StrataNativeHost | undefined { return bridges().__RAINY_STRATA_NATIVE__ }
/** @returns The execution-target bridge, or `undefined` outside the desktop app. */
export function runtimeBridge(): RuntimeNativeHost | undefined { return bridges().__RAINY_RUNTIME_NATIVE__ }
/** @returns The diagnostics export bridge, or `undefined` outside the desktop app. */
export function diagnosticsBridge(): DiagnosticsNativeHost | undefined { return bridges().__RAINY_DIAGNOSTICS__ }
/** @returns The optional-component bridge, or `undefined` outside the desktop app. */
export function modulesBridge(): OptionalModulesBridge | undefined { return bridges().__RAINY_MODULES__ }
