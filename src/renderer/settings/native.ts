/** Electron preload bridges used by Settings; each one is absent when the page runs in a plain browser. */
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { OptionalModulesBridge } from '../../shared/modules-protocol.ts'
import type { StrataNativeHost } from '../../shared/strata-protocol.ts'

/** This Windows, a WSL distribution or a Linux server over SSH that the desktop can run the Host in. */
export interface RuntimeNativeTarget {
  id: string
  kind: 'windows' | 'wsl' | 'ssh'
  label: string
  distro?: string | undefined
  destination?: string | undefined
  port?: number | undefined
  /** An SSH host added in Rainy, which Rainy can also forget; `~/.ssh/config` aliases are not. */
  saved?: boolean | undefined
}

/** The current target and every available one. */
export interface RuntimeNativeTargets { current: RuntimeNativeTarget; targets: RuntimeNativeTarget[] }

/** `window.__RAINY_RUNTIME_NATIVE__`: execution-target switching and offline component preparation. */
export interface RuntimeNativeHost {
  /** @returns The target the Host runs in and every available target. */
  targets(): Promise<RuntimeNativeTargets>
  /**
   * Connect to an SSH host once, checking the login, the system and python3, then remember it.
   * @param request `user@host` or a config alias, and an optional port.
   * @returns The targets, and the id of the added host.
   */
  addSshHost(request: { destination: string; port?: number | undefined }): Promise<RuntimeNativeTargets & { added: string }>
  /** @param targetId An SSH host added in Rainy. @returns The remaining targets. */
  removeSshHost(targetId: string): Promise<RuntimeNativeTargets>
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
