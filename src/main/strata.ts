/** Desktop-owned, offline Strata preparation and serving with external model weights. */
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, stat } from 'node:fs/promises'
import { freemem } from 'node:os'
import { join, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { strataSettingsSchema } from '../shared/strata-protocol.ts'
import type { StrataConnection, StrataStatus } from '../shared/strata-protocol.ts'
import { readStrataHealth } from '../shared/strata-health.ts'
import type { StrataHealth } from '../shared/strata-health.ts'
import { DEFAULT_STRATA_SETTINGS, discoverStrataProfiles, resolveStrataModel } from './strata-model.ts'
import type { ResolvedStrataModel } from './strata-model.ts'
import { spawnStrataProcess } from './strata-process.ts'
import type { StrataProcess, StrataProcessSpec } from './strata-process.ts'
import { writePrivateRecord } from './protected-json.ts'

/** Bundled resources are read-only; all mutable state belongs to this desktop's private user directory. */
export interface StrataManagerOptions {
  readonly runtimeRoot: string
  readonly userData: string
  readonly profileSettingsPath?: string
  readonly startupTimeoutMs?: number
  readonly pollIntervalMs?: number
  readonly ramHeadroomGiB?: number
  readonly platform?: NodeJS.Platform
  readonly launch?: (spec: StrataProcessSpec) => StrataProcess
  readonly readHealth?: (baseURL: string, signal?: AbortSignal) => Promise<StrataHealth>
  readonly unload?: (baseURL: string) => Promise<void>
  readonly availableMemory?: () => number
}

/** Only explicit start requests may prepare files or allocate a model. */
export interface StrataManager {
  /** @returns public configuration, installed-profile choices, and loopback health. */
  status(): Promise<StrataStatus>
  /** @param value - untrusted IPC settings. @returns persisted controls and inspected model files. */
  save(value: unknown): Promise<StrataStatus>
  /** @returns after startup is admitted; status tracks preparation and model loading. */
  start(): Promise<StrataStatus>
  /** @returns after all owned preparation and server processes exit; external servers are never stopped. */
  stop(): Promise<StrataStatus>
  /** @returns a loaded loopback endpoint for the selected Host's independent identity check. */
  connection(): Promise<StrataConnection>
  /** Reject further mutations and await every owned process. */
  close(): Promise<void>
}

const runtimeFiles = ['portablepython/python.exe', 'server/serve/server.py', 'server/tools/iq_pack.py',
  'server/tools/mtp_rt.py', 'server/tools/strata_tokenizer.py', 'server/data/expert-profile.bin', 'engine/strata.exe']

async function available(path: string): Promise<boolean> {
  try { return (await stat(path)).isFile() }
  catch (error) { if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return false; throw error }
}

function childEnvironment(): Record<string, string> {
  const clean = Object.fromEntries(Object.entries(process.env).filter((entry): entry is [string, string] => entry[1] !== undefined
    && !/KEY|SECRET|TOKEN|PASSWORD|^PYTHONPATH$|^PYTHONHOME$|^STRATA_|^NODE_OPTIONS$|^NODE_PATH$/iu.test(entry[0])))
  return { ...clean, PYTHONDONTWRITEBYTECODE: '1', PYTHONNOUSERSITE: '1', HF_HUB_OFFLINE: '1', TRANSFORMERS_OFFLINE: '1',
    HF_DATASETS_OFFLINE: '1' }
}

/**
 * Create a single carrier owner with serialized settings and explicit, cancellable startup.
 * @param options - immutable runtime paths and process/health providers.
 * @returns the native IPC controller; creating it does not read model tensors or start processes.
 */
export function createStrataManager(options: StrataManagerOptions): StrataManager {
  const runtimeRoot = resolve(options.runtimeRoot)
  const stateRoot = resolve(options.userData, 'strata')
  const settingsPath = join(stateRoot, 'settings.json')
  const launch = options.launch ?? spawnStrataProcess
  const readHealth = options.readHealth ?? ((baseURL: string, signal?: AbortSignal) => readStrataHealth(baseURL, { signal }))
  const profileSettingsPath = options.profileSettingsPath ?? (process.env.APPDATA ? join(process.env.APPDATA, 'Strata/settings.json') : undefined)
  let settings = { ...DEFAULT_STRATA_SETTINGS }
  let model: ResolvedStrataModel | null = null
  let phase: StrataStatus['phase'] = 'unconfigured'
  let progress: string | null = null
  let error: string | null = null
  let initialized: Promise<void> | undefined
  let queue: Promise<void> = Promise.resolve()
  let operation: Promise<void> | undefined
  let cancellation: AbortController | undefined
  let owned: StrataProcess | undefined
  let closed = false
  let closing: Promise<void> | undefined
  let health: StrataHealth | null = null

  const baseURL = () => `http://127.0.0.1:${settings.port}/v1`
  const assertOpen = () => { if (closed) throw new Error('RainyAgent 正在关闭，无法修改 Strata。') }
  const busy = () => operation !== undefined || owned !== undefined
  function residentBudget(): number {
    const usableGiB = Math.floor((options.availableMemory ?? freemem)() / 2 ** 30 - (options.ramHeadroomGiB ?? 8))
    if (usableGiB < 1) throw new Error('当前可用内存不足，无法在保留系统内存后加载 Strata。请先保存并结束其他占用内存的工作。')
    if (settings.residentBudgetGiB !== null && settings.residentBudgetGiB > usableGiB) {
      throw new Error(`当前内存最多允许约 ${usableGiB} GiB 的 Strata 驻留预算。请降低高级设置中的内存预算，或先结束其他工作。`)
    }
    return settings.residentBudgetGiB ?? usableGiB
  }
  function transaction<T>(run: () => Promise<T>): Promise<T> {
    const result = queue.then(run)
    queue = result.then(() => {}, () => {})
    return result
  }
  function initialize(): Promise<void> {
    initialized ??= (async () => {
      try {
        if ((await stat(settingsPath)).size > 65536) throw new Error('Strata 设置文件超过 64 KiB。')
        settings = strataSettingsSchema.parse(JSON.parse(await readFile(settingsPath, 'utf8')))
      } catch (failure) { if (!(failure instanceof Error && 'code' in failure && failure.code === 'ENOENT')) throw failure }
      if (settings.modelPath) {
        try {
          model = await resolveStrataModel(settings, stateRoot)
          phase = 'stopped'
          error = model?.missing[0] ?? null
        } catch (failure) { phase = 'error'; error = failure instanceof Error ? failure.message : '模型文件无法读取。' }
      }
    })()
    return initialized
  }
  async function runtime(): Promise<StrataStatus['runtime']> {
    const missing = (await Promise.all(runtimeFiles.map(async path => await available(join(runtimeRoot, path)) ? null : path)))
      .filter((path): path is string => path !== null)
    if ((options.platform ?? process.platform) !== 'win32') missing.unshift('Windows x64 runtime')
    let version: string | null = null
    try {
      const value: unknown = JSON.parse(await readFile(join(runtimeRoot, 'engine/BUILD.json'), 'utf8'))
      if (value !== null && typeof value === 'object' && 'version' in value && typeof value.version === 'string') version = value.version
    } catch (failure) { if (!(failure instanceof Error && 'code' in failure && failure.code === 'ENOENT')) throw failure }
    return { available: missing.length === 0, version, root: runtimeRoot, missing }
  }
  async function probe(signal?: AbortSignal): Promise<StrataHealth | null> {
    try { return await readHealth(baseURL(), signal) }
    catch (failure) { if (signal?.aborted) throw failure; return null }
  }
  async function snapshot(refreshHealth: boolean): Promise<StrataStatus> {
    await initialize()
    if (refreshHealth && phase !== 'preparing' && phase !== 'stopping') {
      const before = { settings, owned, phase }
      const observed = await probe()
      if (settings === before.settings && owned === before.owned && phase === before.phase) health = observed
    }
    const external = !owned && health !== null
    return { phase: external ? 'external' : phase, settings: { ...settings }, runtime: await runtime(),
      profiles: await discoverStrataProfiles(profileSettingsPath), model: model ? { ...model.model } : null,
      server: health ? { ...health, owned: owned !== undefined } : null, progress, error }
  }
  async function startChild(args: readonly string[], logName: string, signal: AbortSignal): Promise<StrataProcess> {
    signal.throwIfAborted()
    assertOpen()
    await mkdir(join(stateRoot, 'logs'), { recursive: true, mode: 0o700 })
    signal.throwIfAborted()
    assertOpen()
    const child = launch({ executable: join(runtimeRoot, 'portablepython/python.exe'), args, cwd: join(runtimeRoot, 'server'),
      environment: childEnvironment(), logPath: join(stateRoot, 'logs', logName) })
    owned = child
    return child
  }
  async function run(selected: ResolvedStrataModel, signal: AbortSignal): Promise<void> {
    for (const preparation of selected.preparations) {
      signal.throwIfAborted()
      phase = 'preparing'
      progress = preparation.stage === 'mtp' ? '正在从本地 MTP 权重准备运行文件…' : '正在从本地 GGUF 准备 Strata 模型文件…'
      await mkdir(preparation.directory, { recursive: true, mode: 0o700 })
      const child = await startChild(['-I', '-B', join(runtimeRoot, 'server/tools', preparation.tool), ...preparation.args], `prepare-${randomUUID()}.log`, signal)
      const result = await child.exited
      if (owned === child) owned = undefined
      signal.throwIfAborted()
      if (result.error || result.code !== 0) throw new Error('本地模型准备失败。请确认所有 GGUF 分片及配套 MTP 属于受支持的 Qwen3.8 Flash Next 模型。'
        + '详细诊断保存在 RainyAgent 的 strata/logs。')
      await writePrivateRecord(join(preparation.directory, '.rainy-complete.json'), Buffer.from(JSON.stringify({ version: 1 }) + '\n'))
    }
    signal.throwIfAborted()
    const ready = await resolveStrataModel(settings, stateRoot)
    if (!ready || ready.preparations.length || ready.missing.length
      || !ready.model.packPath || !ready.model.tokenizerPath || !ready.model.mtpPath) {
      throw new Error('模型文件尚不完整或准备期间发生变化，请检查主模型和配套 MTP 后重试。')
    }
    model = ready
    const configuredResidentBudget = residentBudget()
    const args = ['--pack', ready.model.packPath, '--native', ready.model.ggufPath,
      '--expert-profile', join(runtimeRoot, 'server/data/expert-profile.bin'), '--expert-cache', 'auto', '--prefill', 'auto',
      '--spec', '4', '--spec-min-p', '0.5', '--mtp', ready.model.mtpPath,
      '--max-context', String(settings.contextWindow), '--kv', settings.kvCache,
      '--vram-reserve-mib', String(settings.vramReserveMiB), '--resident-budget-gib', String(configuredResidentBudget)]
    const configPath = join(stateRoot, 'run.json')
    await writePrivateRecord(configPath, Buffer.from(JSON.stringify({ exe: join(runtimeRoot, 'engine/strata.exe'), args,
      cwd: join(runtimeRoot, 'server'), tokenizer: ready.model.tokenizerPath, model_name: ready.model.model,
      log: join(stateRoot, 'logs', `engine-${randomUUID()}.log`),
      host: '127.0.0.1', port: settings.port, open_browser: false, api_monitor: false,
    }, null, 2) + '\n'))
    phase = 'starting'
    progress = '正在加载本地 Strata 模型…'
    const child = await startChild(['-I', '-B', join(runtimeRoot, 'server/serve/server.py'), '--engine', 'strata',
      '--config', configPath, '--host', '127.0.0.1', '--port', String(settings.port)], `server-${randomUUID()}.log`, signal)
    const deadline = Date.now() + (options.startupTimeoutMs ?? 600000)
    while (Date.now() < deadline) {
      signal.throwIfAborted()
      if (child.finished) throw new Error('Strata 服务在就绪前退出。请检查模型文件、显存和端口；诊断保存在 strata/logs。')
      health = await probe(signal)
      if (health?.loaded) {
        if (health.model !== ready.model.model || health.authenticationRequired) throw new Error('端口上的 Strata 服务与本次启动的模型不一致。')
        phase = 'running'
        progress = null
        void child.exited.then(() => {
          if (owned !== child) return
          owned = undefined
          health = null
          if (!signal.aborted) { phase = 'error'; error = 'Strata 服务已退出。模型文件和设置已保留。' }
        })
        return
      }
      await delay(options.pollIntervalMs ?? 1000, undefined, { signal })
    }
    throw new Error('Strata 启动超时。请检查可用显存、内存及模型文件后重试。')
  }
  async function stopOwned(): Promise<void> {
    const pending = operation
    const child = owned
    cancellation?.abort(new Error('Strata 启动或运行已停止。'))
    if (pending || child) phase = 'stopping'
    if (child && !child.finished) {
      if (health?.loaded) {
        try {
          await (options.unload ?? (async (url: string) => {
            await fetch(new URL('/unload', url), { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}',
              signal: AbortSignal.timeout(5000), redirect: 'error' })
          }))(baseURL())
        } catch (_unloadFailure) { /* The owned process must still exit when graceful unloading is unavailable. */ }
      }
      await child.stop()
    }
    await pending
    owned = undefined
    cancellation = undefined
    health = null
    progress = null
    phase = settings.modelPath ? 'stopped' : 'unconfigured'
  }

  return {
    status: () => snapshot(true),
    save: value => transaction(async () => {
      await initialize(); assertOpen()
      if (busy()) throw new Error('请先停止 RainyAgent 启动的 Strata，再修改模型和运行参数。')
      const requested = strataSettingsSchema.parse(value)
      const resolved = await resolveStrataModel(requested, stateRoot, requested.modelPath !== settings.modelPath)
      assertOpen()
      const next = resolved?.settings ?? requested
      await writePrivateRecord(settingsPath, Buffer.from(JSON.stringify(next, null, 2) + '\n'))
      settings = next
      model = resolved
      error = resolved?.missing[0] ?? null
      phase = next.modelPath ? 'stopped' : 'unconfigured'
      health = null
      return snapshot(true)
    }),
    start: () => transaction(async () => {
      await initialize(); assertOpen()
      if (busy()) return snapshot(false)
      const bundled = await runtime()
      assertOpen()
      if (!bundled.available) throw new Error('当前安装包缺少 Strata 运行文件，请安装包含本地模型运行时的完整 RainyAgent。')
      const selected = await resolveStrataModel(settings, stateRoot)
      assertOpen()
      if (!selected) throw new Error('请先选择主模型 GGUF 和配套 MTP 权重。')
      if (selected.missing.length) throw new Error(selected.missing.join('\n'))
      health = await probe()
      assertOpen()
      if (health) {
        if (health.model !== selected.model.model) throw new Error('所选端口已有另一个 Strata 模型；请更换端口，现有服务保持不变。')
        return snapshot(false)
      }
      residentBudget()
      model = selected
      error = null
      phase = selected.preparations.length ? 'preparing' : 'starting'
      const abort = new AbortController()
      cancellation = abort
      operation = Promise.resolve().then(() => run(selected, abort.signal)).catch(async (failure: unknown) => {
        const child = owned
        let stopFailure: unknown
        if (child && !child.finished) {
          try { await child.stop() } catch (failure) { stopFailure = failure }
        }
        if (owned === child && (!child || child.finished)) owned = undefined
        health = null
        progress = null
        if (stopFailure) { phase = 'error'; error = 'Strata 子进程尚未确认退出，请重试停止后再启动。' }
        else if (abort.signal.aborted) phase = 'stopped'
        else { phase = 'error'; error = failure instanceof Error ? failure.message : 'Strata 启动失败。' }
      }).finally(() => { operation = undefined })
      return snapshot(false)
    }),
    stop: () => transaction(async () => { await initialize(); await stopOwned(); return snapshot(true) }),
    connection: async () => {
      await initialize(); assertOpen()
      const current = await readHealth(baseURL())
      if (!current.loaded) throw new Error('Strata 模型尚未加载完成。')
      if (current.authenticationRequired) throw new Error('此服务需要 API 密钥，请在现有模型设置中配置连接。')
      return { baseURL: current.baseURL, model: current.model, contextWindow: current.contextWindow }
    },
    close() {
      closed = true
      cancellation?.abort(new Error('RainyAgent 正在退出。'))
      closing ??= transaction(stopOwned)
      return closing
    },
  }
}
