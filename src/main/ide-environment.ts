/** Explicit human preparation of the selected distribution's offline development tools. */
import { z } from 'zod'

/** Actions the local development setup page may request. Retry is read-only. */
export type IdeEnvironmentAction = { readonly type: 'install' | 'retry' }

/** Read-only executable and distribution observations from the selected WSL environment. */
export interface IdeEnvironmentInspection {
  readonly os: string
  readonly osVersion: string
  readonly architecture: string
  readonly mediaPresent: boolean
  readonly mediaMatches: boolean
  readonly packageCount: number
  readonly incompletePackages: readonly string[]
  readonly tools: readonly { readonly name: string; readonly path: string | null; readonly ready: boolean }[]
}

/** Complete native setup state; a ready result always follows a fresh inspection. */
export interface IdeEnvironmentSnapshot {
  readonly status: 'checking' | 'ready' | 'needs-install' | 'unsupported' | 'missing-media' | 'installing' | 'error'
  readonly distro: string
  readonly message: string
  readonly busy: boolean
  readonly offlineInstallSupported: boolean
  readonly tools: IdeEnvironmentInspection['tools']
  readonly incompletePackages: readonly string[]
  readonly log: readonly string[]
  readonly code?: string
  readonly os?: string
  readonly osVersion?: string
}

/** Platform commands; construction and inspect never install packages or request root. */
export interface IdeEnvironmentPlatform {
  /** @returns read-only state of the selected distribution and shipped package media. */
  readonly inspect: () => Promise<IdeEnvironmentInspection>
  /** @param progress - bounded diagnostic output. @returns completion after the explicit offline installation exits. */
  readonly install: (progress: (message: string) => void) => Promise<void>
}

/** Deployment limits for readonly probes and the retained progress display. */
export const ideEnvironmentConfigSchema = z.object({
  inspectTimeoutMs: z.number().int().positive().default(30000),
  maxOutputCharacters: z.number().int().positive().default(256 * 1024),
  maxProgressLines: z.number().int().positive().default(80),
  maxProgressLineCharacters: z.number().int().positive().default(4000),
}).strict()

/** Complete development setup budgets. */
export type IdeEnvironmentConfig = z.infer<typeof ideEnvironmentConfigSchema>

/**
 * Resolve defaults before constructing a setup owner.
 * @param config - application overrides.
 * @returns complete validated setup limits.
 */
export function resolveIdeEnvironmentConfig(config: z.input<typeof ideEnvironmentConfigSchema> = {}): IdeEnvironmentConfig {
  return ideEnvironmentConfigSchema.parse(config)
}

/** Actionable native setup failure. */
export class IdeEnvironmentError extends Error {
  /** @param code - stable diagnostic code. @param message - context displayed in the local wizard. */
  constructor(readonly code: string, message: string) { super(message); this.name = 'IdeEnvironmentError' }
}

/**
 * Validate local-page JSON without accepting commands, arguments or distribution overrides.
 * @param value - IPC payload.
 * @returns the requested explicit installation or read-only refresh action.
 */
export function parseIdeEnvironmentAction(value: unknown): IdeEnvironmentAction {
  return z.object({ type: z.enum(['install', 'retry']) }).strict().parse(value)
}

/**
 * Check the exact baseline carried by the shipped Ubuntu development layer.
 * @param inspection - observations made in the selected distribution.
 * @returns whether this platform can consume the offline Ubuntu package set.
 */
export function supportsIdeOfflineInstall(inspection: IdeEnvironmentInspection): boolean {
  return inspection.os === 'ubuntu' && inspection.osVersion === '26.04' && inspection.architecture === 'amd64'
}

/** Controller dependencies, with an instance-local progress listener. */
export interface IdeEnvironmentSetupOptions {
  readonly distro: string
  readonly platform: IdeEnvironmentPlatform
  readonly config: IdeEnvironmentConfig
  readonly onProgress?: (snapshot: IdeEnvironmentSnapshot) => void
}

/** Setup lifetime; closing drains an admitted installation rather than interrupting the package manager. */
export interface IdeEnvironmentSetup {
  /** @returns the current observation, coalescing parallel readonly probes. */
  inspect(): Promise<IdeEnvironmentSnapshot>
  /** @param action - validated action. @returns fresh state after the requested action settles. */
  act(action: IdeEnvironmentAction): Promise<IdeEnvironmentSnapshot>
  /** @returns the latest detached progress snapshot. */
  snapshot(): IdeEnvironmentSnapshot
  /** @returns resolution after admitted work drains and future progress notifications are disabled. */
  close(): Promise<void>
}

/**
 * Create a setup controller without inspecting or mutating the selected distribution.
 * @param options - fixed selected distribution, platform owner and progress limits.
 * @returns one serialized explicit-install controller.
 */
export function createIdeEnvironmentSetup(options: IdeEnvironmentSetupOptions): IdeEnvironmentSetup {
  let latest: IdeEnvironmentSnapshot = { status: 'checking', distro: options.distro, message: '正在检查开发工具…', busy: false,
    offlineInstallSupported: false, tools: [], incompletePackages: [], log: [] }
  let action: Promise<IdeEnvironmentSnapshot> | undefined
  let inspection: Promise<IdeEnvironmentSnapshot> | undefined
  let closing = false
  let disposal: Promise<void> | undefined

  const snapshot = (): IdeEnvironmentSnapshot => structuredClone(latest)
  const publish = (update: Partial<IdeEnvironmentSnapshot>): IdeEnvironmentSnapshot => {
    latest = { ...latest, ...update }
    if (!closing) {
      try { options.onProgress?.(snapshot()) }
      catch (error) { console.error('IDE environment progress listener failed', error) }
    }
    return snapshot()
  }
  const fail = (error: unknown): IdeEnvironmentSnapshot => publish({ status: 'error', busy: false,
    code: error instanceof IdeEnvironmentError ? error.code : 'development-setup-failed',
    message: error instanceof Error ? error.message : '开发工具准备失败，请重新检查后重试。' })

  const inspectInternal = async (): Promise<IdeEnvironmentSnapshot> => {
    try {
      const observed = await options.platform.inspect()
      const supported = supportsIdeOfflineInstall(observed)
      const missing = observed.tools.filter(tool => !tool.ready)
      const incomplete = supported && observed.mediaMatches ? observed.incompletePackages : []
      publish({ tools: observed.tools, incompletePackages: incomplete, offlineInstallSupported: supported,
        os: observed.os, osVersion: observed.osVersion, busy: false, code: undefined })
      if (missing.length === 0 && incomplete.length === 0) return publish({ status: 'ready', message: '开发工具已就绪，可以返回工作区运行和调试代码。' })
      if (!supported) return publish({ status: 'unsupported', code: 'unsupported-distribution',
        message: `所选环境为 ${observed.os} ${observed.osVersion}（${observed.architecture}）。随包离线安装仅支持 Ubuntu 26.04 amd64；请在所选环境中手动准备缺失工具。` })
      if (!observed.mediaPresent || !observed.mediaMatches) return publish({ status: 'missing-media', code: 'development-media-missing',
        message: '离线开发工具文件缺失或不匹配，请重新安装完整的 RainyAgent 安装包后重试。' })
      return publish({ status: 'needs-install', message: missing.length > 0
        ? `缺少 ${missing.map(tool => tool.name).join('、')}。点击“安装开发工具”后，将在当前环境中安装随包提供的开发组件。`
        : '开发组件尚未完成配置。点击“安装开发工具”可继续离线准备。' })
    } catch (error) { return fail(error) }
  }

  const inspect = (): Promise<IdeEnvironmentSnapshot> => {
    if (closing) return Promise.reject(new IdeEnvironmentError('setup-closed', '开发工具向导已关闭。'))
    if (action !== undefined) return Promise.resolve(snapshot())
    inspection ??= inspectInternal().finally(() => { inspection = undefined })
    return inspection
  }

  const perform = async (requested: IdeEnvironmentAction): Promise<IdeEnvironmentSnapshot> => {
    await inspection
    if (requested.type === 'retry') return inspectInternal()
    try {
      const before = await inspectInternal()
      if (before.status !== 'needs-install') return before
      publish({ status: 'installing', busy: true, code: undefined, log: [], message: '正在校验并离线安装开发工具，请等待完成后再关闭此窗口。' })
      await options.platform.install((message) => {
        const lines = message.replaceAll('\r', '').split('\n').filter(line => line.trim() !== '')
          .map(line => line.slice(-options.config.maxProgressLineCharacters))
        publish({ log: [...latest.log, ...lines].slice(-options.config.maxProgressLines) })
      })
      const after = await inspectInternal()
      if (after.status === 'needs-install') return publish({ status: 'error', code: 'development-tools-incomplete', busy: false,
        message: '安装已退出，但部分开发工具仍未就绪。请查看安装详情后重新检查，再明确选择安装。' })
      return after
    } catch (error) { return fail(error) }
  }

  const act = (requested: IdeEnvironmentAction): Promise<IdeEnvironmentSnapshot> => {
    if (closing) return Promise.reject(new IdeEnvironmentError('setup-closed', '开发工具向导已关闭。'))
    if (action !== undefined) return action
    action = perform(requested).finally(() => { action = undefined })
    return action
  }

  return { inspect, act, snapshot, close: () => {
    closing = true
    disposal ??= Promise.allSettled([action, inspection]).then(() => undefined)
    return disposal
  } }
}
