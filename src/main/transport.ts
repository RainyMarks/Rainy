/** Owned native and WSL Host lifecycle, private control requests and bounded startup. */
import { execFile, spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { createInterface } from 'node:readline'
import { createConnection } from 'node:net'
import { setTimeout as delay } from 'node:timers/promises'
import { randomUUID } from 'node:crypto'
import { posix, win32 } from 'node:path'
import { z } from 'zod'
import { brandString } from '../shared/brand.ts'
import { ProjectId } from '../shared/project-registry.ts'
import type { IdeRootId, WorkspaceId } from '../shared/ide-files-protocol.ts'

/** Host-authoritative roots and stable project identity used during a target switch. */
export interface HostProjectSnapshot {
  projectId: ProjectId
  workspaceId: WorkspaceId
  roots: readonly { rootId: IdeRootId; path: string; title: string; primary: boolean }[]
}
const projectSchema: z.ZodType<HostProjectSnapshot> = z.object({
  projectId: z.string().transform(ProjectId),
  workspaceId: z.string().transform(value => brandString<WorkspaceId>(value)),
  roots: z.array(z.object({
    rootId: z.string().min(1).transform(value => brandString<IdeRootId>(value)),
    path: z.string().min(1), title: z.string(), primary: z.boolean(),
  }).strict()),
}).strict()

/** Private Host readiness, compatible with existing packaged WSL runtimes. */
export interface HostReady { protocol: 1; url: string; pid: number; home: string }
/** The carrier owns one active execution target at a time. */
export interface HostTransport {
  /** @returns readiness after profile loading and, for WSL, Windows-side loopback reachability. */
  start(): Promise<HostReady>
  /** @param mode - observe activity, freeze new execution when idle, or cancel a previous freeze. @returns current owned activity. */
  inspectActivity(mode?: 'observe' | 'freeze' | 'resume'): Promise<{ active: boolean }>
  /**
   * @param workspaceId - explicit workspace, or the Host's saved selection when omitted.
   * @returns canonical project roots, or null when no project is selected.
   */
  inspectProject(workspaceId?: WorkspaceId): Promise<HostProjectSnapshot | null>
  /** @returns only after the owned Host carrier has exited. */
  stop(): Promise<void>
}
/** Explicit runtime entry and callbacks shared by both carriers. */
export interface HostOptions {
  entry: string
  node: string
  environment?: Readonly<Record<string, string>>
  configureDeepSeek?: boolean
  idaMcpCommand?: string
  onDiagnostic?: (text: string) => void
  onExit?: (code: number | null) => void
}
/** Selected registered WSL distribution; arguments are never shell-interpolated. */
export interface WslHostOptions extends HostOptions { distro: string }
/** Native Host working directory, separate from every project directory. */
export interface WindowsHostOptions extends HostOptions { cwd: string }

/** @param value - untrusted child control message. @returns verified loopback readiness. */
export function parseReady(value: unknown): HostReady {
  if (value === null || typeof value !== 'object' || !('protocol' in value) || value.protocol !== 1
    || !('url' in value) || typeof value.url !== 'string' || !('pid' in value) || !Number.isSafeInteger(value.pid)
    || Number(value.pid) < 1 || !('home' in value) || typeof value.home !== 'string'
    || (!posix.isAbsolute(value.home) && !win32.isAbsolute(value.home)) || value.home.includes('\0')) throw new Error('Host protocol does not match this application.')
  const url = new URL(value.url)
  if (url.protocol !== 'http:' || url.hostname !== '127.0.0.1' || !url.port || url.username || url.password) throw new Error('Host returned an invalid application URL.')
  return { protocol: 1, url: value.url, pid: Number(value.pid), home: value.home }
}

function environment(options: HostOptions): Record<string, string> {
  return { DSH_TELEMETRY_DISABLED: '1',
    ...(options.configureDeepSeek ? { RAINY_CONFIGURE_DEEPSEEK: '1' } : {}),
    ...(options.idaMcpCommand ? { RAINY_IDA_MCP_COMMAND: options.idaMcpCommand } : {}), ...options.environment }
}

/** Preserve the startup owner's failure when cancellation interrupts an owned operation. */
function startupError(signal: AbortSignal): Error {
  return signal.reason instanceof Error ? signal.reason : new Error('The execution Host startup was cancelled.')
}

/** One TCP attempt settles only after its owned socket closes. */
function connectLoopback(port: number, signal: AbortSignal): Promise<void> {
  signal.throwIfAborted()
  return new Promise((resolve, reject) => {
    const socket = createConnection({ host: '127.0.0.1', port })
    let connected = false
    let failure: Error | undefined
    const abort = () => { socket.destroy() }
    socket.once('connect', () => { connected = true; socket.destroy() })
    socket.once('error', (error) => { failure = error; socket.destroy() })
    socket.once('close', () => {
      signal.removeEventListener('abort', abort)
      if (signal.aborted) reject(startupError(signal))
      else if (failure) reject(failure)
      else if (connected) resolve()
      else reject(new Error('The Host loopback connection closed before connecting.'))
    })
    signal.addEventListener('abort', abort, { once: true })
    if (signal.aborted) abort()
  })
}

/** WSL can report Linux readiness before Windows localhost forwarding accepts connections. */
async function waitForWindowsLoopback(host: HostReady, signal: AbortSignal): Promise<void> {
  const port = Number(new URL(host.url).port)
  for (;;) {
    signal.throwIfAborted()
    try {
      await connectLoopback(port, signal)
      return
    } catch (error) {
      signal.throwIfAborted()
      if (!(error instanceof Error) || !('code' in error) || error.code !== 'ECONNREFUSED') throw error
    }
    await delay(100, undefined, { signal })
  }
}

interface PendingRequest<T> {
  resolve: (value: T) => void
  reject: (error: Error) => void
  timer: ReturnType<typeof setTimeout>
}

/** Remove one pending control request so exactly one settlement path owns it. */
function takeRequest<T>(requests: Map<string, PendingRequest<T>>, id: string): PendingRequest<T> | undefined {
  const request = requests.get(id)
  if (request === undefined) return undefined
  clearTimeout(request.timer)
  requests.delete(id)
  return request
}

function rejectAll<T>(requests: Map<string, PendingRequest<T>>, error: Error): void {
  for (const id of [...requests.keys()]) takeRequest(requests, id)?.reject(error)
}

abstract class ManagedHostTransport implements HostTransport {
  protected child?: ChildProcessWithoutNullStreams
  protected host?: HostReady
  private exit?: Promise<void>
  private ready?: Promise<HostReady>
  private stopping?: Promise<void>
  private readonly startup = new AbortController()
  private startupProbe?: Promise<void>
  private readonly requests = new Map<string, PendingRequest<{ active: boolean }>>()
  private readonly projectRequests = new Map<string, PendingRequest<HostProjectSnapshot | null>>()
  constructor(protected readonly options: HostOptions) {}
  protected abstract launch(): ChildProcessWithoutNullStreams
  protected abstract forceStop(child: ChildProcessWithoutNullStreams): void
  /** Native readiness has no cross-OS forwarding step. */
  protected confirmReady(_host: HostReady, _signal: AbortSignal): Promise<void> { return Promise.resolve() }
  private diagnostic(text: string): void {
    try { this.options.onDiagnostic?.(text) } catch (error) { console.error('Host diagnostic callback failed:', error) }
  }
  start(): Promise<HostReady> {
    if (this.ready) return this.ready
    if (this.stopping) return Promise.reject(new Error('The execution Host has already stopped.'))
    this.ready = new Promise((resolve, reject) => {
      const child = this.launch()
      this.child = child
      const fail = (error: Error): void => {
        this.startup.abort(error)
        void this.stop().then(() => { reject(startupError(this.startup.signal)) }, reject)
      }
      const timer = setTimeout(() => { fail(new Error('The execution Host did not start before its deadline.')) }, 90000)
      this.startup.signal.addEventListener('abort', () => { clearTimeout(timer) }, { once: true })
      const lines = createInterface({ input: child.stdout, crlfDelay: Infinity })
      lines.on('line', (line) => {
        if (!line.startsWith('RAINY_CONTROL ')) { this.diagnostic(line); return }
        try {
          const data: unknown = JSON.parse(line.slice(14))
          if (data === null || typeof data !== 'object' || !('type' in data)) throw new Error('Invalid Host control message.')
          if (data.type === 'ready') {
            if (this.host || this.startup.signal.aborted) return
            const host = parseReady(data)
            this.host = host
            this.startupProbe = this.confirmReady(host, this.startup.signal)
            void this.startupProbe.then(() => {
              if (this.startup.signal.aborted || this.stopping) return
              clearTimeout(timer)
              resolve(host)
            }, (error: unknown) => { fail(error instanceof Error ? error : new Error('Host readiness failed.')) })
          }
          else if (data.type === 'fatal') { fail(new Error('message' in data ? String(data.message) : 'Host startup failed.')) }
          else if (data.type === 'activity' && 'id' in data && typeof data.id === 'string') {
            const request = takeRequest(this.requests, data.id)
            if (!request) return
            if ('active' in data && typeof data.active === 'boolean') request.resolve({ active: data.active })
            else request.reject(new Error('Host activity inspection returned an invalid response.'))
          }
          else if (data.type === 'project' && 'id' in data && typeof data.id === 'string') {
            const request = takeRequest(this.projectRequests, data.id)
            if (!request) return
            const parsed = 'project' in data ? projectSchema.safeParse(data.project) : undefined
            if ('project' in data && data.project === null) request.resolve(null)
            else if (parsed?.success) request.resolve(parsed.data)
            else request.reject(new Error('The Host could not resolve this project for target migration.'))
          }
        } catch (error) { fail(error instanceof Error ? error : new Error('Invalid Host response.')) }
      })
      child.stderr.setEncoding('utf8')
      child.stderr.on('data', (text) => { this.diagnostic(String(text)) })
      this.exit = new Promise((done) => {
        child.once('close', (code) => {
          clearTimeout(timer); lines.close()
          rejectAll(this.requests, new Error('The execution Host stopped.'))
          rejectAll(this.projectRequests, new Error('The execution Host stopped.'))
          try { this.options.onExit?.(code) } catch (error) { console.error('Host exit callback failed:', error) }
          done()
          fail(new Error(`The execution Host stopped (${code}).`))
        })
        child.once('error', fail)
      })
    })
    return this.ready
  }
  /** Send one control request whose reply is matched by id on the Host's stdout control channel. */
  private request<T>(requests: Map<string, PendingRequest<T>>, message: Record<string, unknown>,
    unavailable: string, timedOut: string): Promise<T> {
    const child = this.child
    if (!this.host || !child?.stdin.writable || this.stopping) return Promise.reject(new Error(unavailable))
    const id = randomUUID()
    const { promise, resolve, reject } = Promise.withResolvers<T>()
    const timer = setTimeout(() => { takeRequest(requests, id)?.reject(new Error(timedOut)) }, 10000)
    requests.set(id, { resolve, reject, timer })
    child.stdin.write(JSON.stringify({ ...message, id }) + '\n', (error) => {
      if (error) takeRequest(requests, id)?.reject(error)
    })
    return promise
  }
  inspectActivity(mode: 'observe' | 'freeze' | 'resume' = 'observe'): Promise<{ active: boolean }> {
    return this.request(this.requests, { type: 'inspect-activity', mode },
      'The execution Host is unavailable for activity inspection.', 'Host activity inspection timed out; the target was not switched.')
  }
  inspectProject(workspaceId?: WorkspaceId): Promise<HostProjectSnapshot | null> {
    return this.request(this.projectRequests, { type: 'inspect-project', workspaceId },
      'The execution Host is unavailable.', 'Project target inspection timed out.')
  }
  stop(): Promise<void> {
    this.stopping ??= (async () => {
      this.startup.abort(new Error('The execution Host stopped before readiness completed.'))
      const child = this.child
      if (!child) return
      let timer: ReturnType<typeof setTimeout> | undefined
      if (child.exitCode === null && child.signalCode === null) {
        if (child.stdin.writable) child.stdin.end(JSON.stringify({ type: 'stop' }) + '\n')
        timer = setTimeout(() => {
          try { this.forceStop(child) }
          catch (error) { this.diagnostic(error instanceof Error ? error.message : 'Host force-stop failed.') }
        }, 8000)
      }
      try { await Promise.allSettled([this.exit, this.startupProbe]) }
      finally { if (timer) clearTimeout(timer) }
    })()
    return this.stopping
  }
}

/** Native Windows Host with the same profile/control protocol as the WSL carrier. */
export class WindowsHostTransport extends ManagedHostTransport {
  private readonly settings: WindowsHostOptions
  /** @param settings - explicit native runtime and application-owned working directory. */
  constructor(settings: WindowsHostOptions) {
    super(settings)
    this.settings = settings
  }
  protected launch(): ChildProcessWithoutNullStreams {
    const clean = Object.fromEntries(Object.entries(process.env).filter(([key, value]) =>
      value !== undefined && !/KEY|SECRET|TOKEN|PASSWORD|^NODE_OPTIONS$|^NODE_PATH$/iu.test(key)))
    return spawn(this.settings.node, ['--expose-internals', this.settings.entry], { cwd: this.settings.cwd,
      env: { ...clean, ...environment(this.settings) }, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] })
  }
  protected forceStop(child: ChildProcessWithoutNullStreams): void {
    if (child.exitCode !== null || child.signalCode !== null || child.pid === undefined) return
    execFile('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, timeout: 5000 }, () => { if (child.exitCode === null) child.kill() })
  }
}

/** WSL carrier starting the verified Linux Host inside one explicitly selected distribution. */
export class WslHostTransport extends ManagedHostTransport {
  private readonly settings: WslHostOptions
  /** @param settings - registered distribution and absolute Linux runtime paths. */
  constructor(settings: WslHostOptions) {
    super(settings)
    this.settings = settings
  }
  protected override confirmReady(host: HostReady, signal: AbortSignal): Promise<void> {
    return waitForWindowsLoopback(host, signal)
  }
  protected launch(): ChildProcessWithoutNullStreams {
    return spawn('wsl.exe', ['--distribution', this.settings.distro, '--exec', 'env',
      ...Object.entries(environment(this.settings)).map(([key, value]) => `${key}=${value}`),
      this.settings.node, '--expose-internals', this.settings.entry], { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] })
  }
  protected forceStop(child: ChildProcessWithoutNullStreams): void {
    if (!this.host) { child.kill(); return }
    const checkAndStop = 'import os,sys,signal; p=int(sys.argv[1]); f="/proc/%d/cmdline"%p; a=open(f,"rb").read().split(b"\\0") if os.path.exists(f) else []; os.kill(p,signal.SIGTERM) if len(a)>2 and a[0].decode()==sys.argv[2] and a[2].decode()==sys.argv[3] else None'
    execFile('wsl.exe', ['-d', this.settings.distro, '--exec', 'python3', '-c', checkAndStop, String(this.host.pid), this.settings.node, this.settings.entry], { windowsHide: true, timeout: 3000 }, () => child.kill())
  }
}
