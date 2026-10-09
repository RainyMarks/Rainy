/** Offline desktop environment setup, with durable progress and explicit system-change actions. */
import { createHash, randomUUID } from 'node:crypto'
import { mkdir, open, readFile, rename } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { z } from 'zod'
import { createWindowsEnvironmentPlatform } from './environment-platform.ts'

/** A registered Linux distribution and the directory Windows associates with it. */
export interface EnvironmentDistribution { name: string; version: number; basePath: string }

/** Read-only observations used to select the next setup action. */
export interface EnvironmentSystem {
  supported: boolean
  reason?: string
  virtualization: boolean
  wslInstalled: boolean
  componentsEnabled: boolean
  bootId: string
  distributions: EnvironmentDistribution[]
}

/** Setup actions accepted from the local page. */
export type EnvironmentAction =
  | { type: 'retry' | 'resume' | 'install-system-components' | 'create-managed-distro' }
  | { type: 'select-existing'; distroName: string }

/** Complete page state. The ready state always identifies a health-checked distribution. */
export interface EnvironmentSnapshot {
  status: 'ready' | 'needs-system' | 'needs-distro' | 'saved-distro-missing' | 'reboot-required' | 'blocked' | 'error' | 'working'
  message: string
  code?: string
  distro?: string
  savedDistro?: string
  distributions: EnvironmentDistribution[]
  installRoot: string
  managedDistro: string
  busy: boolean
  canResume?: boolean
}

/** Platform operations are injected in recovery tests; only installSystem requests elevation. */
export interface EnvironmentPlatform {
  inspectSystem: () => Promise<EnvironmentSystem>
  checkDistribution: (name: string) => Promise<void>
  acquireLock: () => Promise<() => Promise<void>>
  installSystem: (progress: (message: string) => void) => Promise<{ rebootRequired: boolean }>
  createManagedDistribution: (name: string, progress: (message: string) => void) => Promise<void>
}

/** Host integration uses existing settings persistence without replacing unrelated preferences. */
export interface EnvironmentSetupOptions {
  installRoot: string
  userData: string
  mediaRoot?: string
  resolveMediaRoot?: () => Promise<string>
  readDesktopSettings(): Promise<Record<string, unknown>>
  writeDesktopSettings(settings: Record<string, unknown>): Promise<void>
  onProgress?(snapshot: EnvironmentSnapshot): void
  platform?: EnvironmentPlatform
}

const journalSchema = z.object({
  version: z.literal(1),
  installRoot: z.string(),
  managedDistro: z.string(),
  phase: z.enum(['installing-system', 'system-ready', 'awaiting-reboot', 'creating-distro', 'ready']),
  bootId: z.string().optional(),
  distro: z.string().optional(),
}).strict()
type Journal = z.infer<typeof journalSchema>

/** An actionable setup failure, including platform error codes safe for the local page. */
export class EnvironmentSetupError extends Error {
  constructor(readonly code: string, message: string) { super(message); this.name = 'EnvironmentSetupError' }
}

/** Stable directory identity for installation-specific recovery records.
 * @param installRoot Absolute or relative application installation directory.
 * @returns The case-insensitive Windows installation identifier.
 */
export function environmentInstallationId(installRoot: string): string {
  return createHash('sha256').update(resolve(installRoot).toLowerCase()).digest('hex').slice(0, 12)
}

/** Keep setup progress and elevated-install results separate for each installation directory.
 * @param installRoot Application installation directory.
 * @param userData Shared desktop preference directory.
 * @returns The private recovery directory for this installation.
 */
export function environmentStateDirectory(installRoot: string, userData: string): string {
  return join(userData, 'environment', environmentInstallationId(installRoot))
}

/** Atomically replace a setup record without destroying the previous file on write failure.
 * @param path Destination in the installation's private data directory.
 * @param value JSON-serializable record.
 * @returns Completion after the replacement has been persisted.
 */
export async function writeEnvironmentRecord(path: string, value: object): Promise<void> {
  const temporary = `${path}.${randomUUID()}.tmp`
  const file = await open(temporary, 'wx', 0o600)
  try {
    await file.writeFile(JSON.stringify(value, null, 2) + '\n')
    await file.sync()
  } finally { await file.close() }
  await rename(temporary, path)
}

/** Create one serialized setup controller; no Windows changes occur during inspection.
 * @param options Installation paths, settings storage, and progress listener.
 * @returns The read-only inspector and explicit action dispatcher.
 */
export function createEnvironmentSetup(options: EnvironmentSetupOptions): {
  inspect(): Promise<EnvironmentSnapshot>
  act(action: EnvironmentAction): Promise<EnvironmentSnapshot>
} {
  const installRoot = resolve(options.installRoot)
  const id = environmentInstallationId(installRoot)
  const managedDistro = `RainyAgent-${id}`
  const journalDirectory = environmentStateDirectory(installRoot, options.userData)
  const journalPath = join(journalDirectory, 'setup.json')
  const legacyJournalPath = join(options.userData, 'environment', 'setup.json')
  const platform = options.platform ?? createWindowsEnvironmentPlatform({
    installRoot, userData: options.userData, mediaRoot: options.mediaRoot, resolveMediaRoot: options.resolveMediaRoot,
  })
  let actionPromise: Promise<EnvironmentSnapshot> | undefined
  let inspection: Promise<EnvironmentSnapshot> | undefined
  let latest: EnvironmentSnapshot = { status: 'working', message: '正在检查本机环境…', distributions: [], installRoot, managedDistro, busy: false }

  function publish(update: Partial<EnvironmentSnapshot>): EnvironmentSnapshot {
    latest = { ...latest, ...update }
    try { options.onProgress?.(latest) } catch (error) { console.error('Environment progress listener failed', error) }
    return latest
  }

  async function readJournal(): Promise<Journal | undefined> {
    let text: string
    try { text = await readFile(journalPath, 'utf8') } catch (error) {
      if (error instanceof Error && 'code' in error && error.code === 'ENOENT') {
        let legacy: unknown
        try { legacy = JSON.parse(await readFile(legacyJournalPath, 'utf8')) } catch (legacyError) {
          if (legacyError instanceof SyntaxError || legacyError instanceof Error && 'code' in legacyError && legacyError.code === 'ENOENT') return undefined
          throw legacyError
        }
        const previous = journalSchema.safeParse(legacy)
        const belongsToInstallation = previous.success && environmentInstallationId(previous.data.installRoot) === id
          && previous.data.managedDistro === managedDistro
        return previous.success && belongsToInstallation ? previous.data : undefined
      }
      throw error
    }
    let record: unknown
    try { record = JSON.parse(text) } catch (error) { throw new EnvironmentSetupError('journal-invalid', `环境安装记录无法读取。请保留 ${journalPath} 并联系支持人员。${error instanceof Error ? ' ' + error.message : ''}`) }
    const parsed = journalSchema.safeParse(record)
    if (!parsed.success || environmentInstallationId(parsed.data.installRoot) !== id || parsed.data.managedDistro !== managedDistro) {
      throw new EnvironmentSetupError('journal-invalid', `环境安装记录损坏或属于其他安装目录。请保留 ${journalPath} 并联系支持人员。`)
    }
    return parsed.data
  }

  async function saveJournal(phase: Journal['phase'], fields: Partial<Pick<Journal, 'bootId' | 'distro'>> = {}): Promise<void> {
    await mkdir(journalDirectory, { recursive: true })
    await writeEnvironmentRecord(journalPath, { version: 1, installRoot, managedDistro, phase, ...fields })
  }

  function failed(error: unknown): EnvironmentSnapshot {
    return publish({ status: 'error', busy: false, code: error instanceof EnvironmentSetupError ? error.code : 'setup-failed', message: error instanceof Error ? error.message : '环境检查失败，请重试。' })
  }

  async function inspectInternal(): Promise<EnvironmentSnapshot> {
    try {
      const [system, settings, journal] = await Promise.all([platform.inspectSystem(), options.readDesktopSettings(), readJournal()])
      const saved = settings.distro
      if (saved !== undefined && (typeof saved !== 'string' || !saved.trim())) throw new EnvironmentSetupError('settings-invalid', 'desktop.json 中的发行版名称无效，请恢复配置后重试。')
      const savedDistro = typeof saved === 'string' ? saved : undefined
      publish({ distributions: system.distributions, savedDistro, code: undefined, distro: undefined, busy: false, canResume: journal?.phase === 'creating-distro' })
      if (!system.supported) return publish({ status: 'blocked', code: 'unsupported-platform', message: system.reason ?? '需要 Windows 10 22H2 或更新版本的 64 位系统。' })
      if (journal?.phase === 'awaiting-reboot' && journal.bootId === system.bootId) {
        return publish({ status: 'reboot-required', message: '系统组件已安装。请先保存工作并重启 Windows，再打开 RainyAgent 继续。' })
      }
      if (journal?.phase === 'installing-system' && journal.bootId === system.bootId && system.wslInstalled && system.componentsEnabled) {
        return publish({ status: 'reboot-required', message: '上次系统安装后尚未确认重启。请保存工作并重启 Windows，再打开 RainyAgent 继续。' })
      }
      if (savedDistro && system.distributions.some(distro => distro.name === savedDistro && distro.version === 2)) {
        await platform.checkDistribution(savedDistro)
        return publish({ status: 'ready', distro: savedDistro, message: '运行环境已就绪。' })
      }
      if (!system.virtualization) return publish({ status: 'blocked', code: 'virtualization-disabled', message: '请在 BIOS / UEFI 中启用 CPU 虚拟化（Intel VT-x 或 AMD SVM），启动 Windows 后重试。' })
      if (!system.wslInstalled || !system.componentsEnabled) return publish({ status: 'needs-system', message: '需要安装 WSL 和 Windows 虚拟化组件。安装将请求管理员权限，可能需要重启。' })
      if (savedDistro) return publish({ status: 'saved-distro-missing', message: `原环境“${savedDistro}”暂时不可用。恢复该环境后重试，或明确选择其他环境。新建环境不会恢复原聊天数据。` })
      return publish({ status: 'needs-distro', message: journal?.phase === 'creating-distro' ? '上次环境创建尚未完成，可以继续恢复。' : '将从随安装包提供的 Ubuntu 镜像创建 RainyAgent 专用环境。' })
    } catch (error) { return failed(error) }
  }

  async function inspect(): Promise<EnvironmentSnapshot> {
    if (actionPromise) return latest
    inspection ??= inspectInternal().finally(() => { inspection = undefined })
    return inspection
  }

  async function select(name: string): Promise<EnvironmentSnapshot> {
    const system = await platform.inspectSystem()
    if (!system.distributions.some(distro => distro.name === name && distro.version === 2)) throw new EnvironmentSetupError('distro-missing', '所选 WSL2 环境已不可用，请刷新列表。')
    await platform.checkDistribution(name)
    const settings = await options.readDesktopSettings()
    await options.writeDesktopSettings({ ...settings, distro: name })
    await saveJournal('ready', { distro: name })
    return publish({ status: 'ready', distro: name, savedDistro: name, busy: false, canResume: false, message: '运行环境已就绪。', code: undefined })
  }

  async function perform(action: EnvironmentAction): Promise<EnvironmentSnapshot> {
    await inspection
    if (action.type === 'retry') return inspectInternal()
    let release: (() => Promise<void>) | undefined
    try {
      release = await platform.acquireLock()
      const previous = await inspectInternal()
      if (previous.status === 'ready' && action.type !== 'select-existing') return previous
      if (previous.status === 'blocked' || previous.status === 'reboot-required' || previous.code === 'journal-invalid') return previous
      if (previous.status === 'error' && previous.code !== 'distro-unhealthy') return previous
      if (action.type === 'install-system-components' && previous.status !== 'needs-system') return previous
      publish({ status: 'working', busy: true, message: '正在准备环境…', code: undefined })
      const progress = (message: string): void => { publish({ message, busy: true }) }
      if (action.type === 'select-existing') return await select(action.distroName)
      if (action.type === 'install-system-components') {
        const system = await platform.inspectSystem()
        await saveJournal('installing-system', { bootId: system.bootId })
        const result = await platform.installSystem(progress)
        if (result.rebootRequired) {
          await saveJournal('awaiting-reboot', { bootId: system.bootId })
          return publish({ status: 'reboot-required', busy: false, message: '系统组件已安装。请保存工作并重启 Windows，再打开 RainyAgent 继续。' })
        }
        await saveJournal('system-ready')
        return await inspectInternal()
      }
      const system = await platform.inspectSystem()
      if (!system.wslInstalled || !system.componentsEnabled) return publish({ status: 'needs-system', busy: false, message: '请先安装系统组件。' })
      await saveJournal('creating-distro')
      await platform.createManagedDistribution(managedDistro, progress)
      return await select(managedDistro)
    } catch (error) { return failed(error) } finally { await release?.() }
  }

  async function act(action: EnvironmentAction): Promise<EnvironmentSnapshot> {
    if (actionPromise) return actionPromise
    actionPromise = perform(action).finally(() => { actionPromise = undefined })
    return actionPromise
  }
  return { inspect, act }
}
