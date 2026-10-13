/** Child processes and pseudo-terminals with whole-tree termination on Windows and Linux. */
import { spawn as spawnChild, type ChildProcess } from 'node:child_process'
import { accessSync, constants, lstatSync, statSync } from 'node:fs'
import { basename, delimiter, isAbsolute, join } from 'node:path'
import { PassThrough, type Readable, type Writable } from 'node:stream'
import { createRequire } from 'node:module'
import type { IPty } from 'node-pty'
import { localized } from '../shared/locale.ts'

/** Environment names never inherited by child processes. */
export const SENSITIVE_ENV_PATTERN = /KEY|PASSWORD|SECRET|TOKEN/i

/**
 * The Host's environment without credential-shaped variables.
 * @param base Environment to copy; defaults to `process.env`.
 * @returns A new environment object.
 */
export function scrubbedEnv(base: NodeJS.ProcessEnv = process.env): Record<string, string> {
  const result: Record<string, string> = {}
  for (const [name, value] of Object.entries(base)) {
    if (value !== undefined && !SENSITIVE_ENV_PATTERN.test(name)) result[name] = value
  }
  return result
}

/** Raised when a program name cannot be found on PATH. */
export class ExecutableNotFoundError extends Error {
  /** @param command The program that was looked up. */
  constructor(readonly command: string) {
    super(localized({ zh: `找不到可执行程序：${command}。请在 设置 → 运行环境 中选择或准备它。`, en: `Executable not found: ${command}. Choose or prepare it in Settings → Runtime.` }))
    this.name = 'ExecutableNotFoundError'
  }
}

function pathValue(env: NodeJS.ProcessEnv): string {
  if (process.platform !== 'win32') return env.PATH ?? ''
  const key = Object.keys(env).find(name => name.toUpperCase() === 'PATH')
  return key === undefined ? '' : env[key] ?? ''
}

function executable(path: string): boolean {
  try {
    if (!statSync(path).isFile()) return false
    if (process.platform !== 'win32') accessSync(path, constants.X_OK)
    return true
  } catch (_error) {
    return false
  }
}

/**
 * Find a program the way the platform shell would.
 * @param command Program name or path.
 * @param env Environment whose PATH (and PATHEXT on Windows) is searched.
 * @returns Absolute path of the program, or `undefined`.
 */
export function resolveExecutable(command: string, env: NodeJS.ProcessEnv = process.env): string | undefined {
  const extensions = process.platform === 'win32'
    ? ['', ...(env.PATHEXT ?? '.COM;.EXE;.BAT;.CMD').split(';').filter(Boolean).map(ext => ext.toLowerCase())]
    : ['']
  if (isAbsolute(command) || command.includes('/') || (process.platform === 'win32' && command.includes('\\'))) {
    return extensions.map(ext => command + ext).find(executable)
  }
  for (const directory of pathValue(env).split(delimiter)) {
    if (directory === '') continue
    for (const ext of extensions) {
      const candidate = join(directory, command + ext)
      if (executable(candidate)) return candidate
    }
  }
  return undefined
}

/** A PowerShell executable: PowerShell 7 or later (`core`) or Windows PowerShell 5.1 (`desktop`). */
export interface PowerShellChoice {
  path: string
  edition: 'core' | 'desktop'
}

function spawnable(candidate: string): boolean {
  try {
    // lstat sees the Microsoft Store execution alias, which stat reports as inaccessible.
    const info = lstatSync(candidate)
    return info.isFile() || info.isSymbolicLink()
  } catch (_absentCandidate) {
    return false
  }
}

/**
 * Choose the PowerShell of the Windows command tool and IDE terminals. RainyAgent does not ship PowerShell: an explicit
 * path wins, then PowerShell 7 from the Windows basic component, Program Files or PATH, and finally the Windows
 * PowerShell 5.1 that every Windows installation has.
 * @param options.configured Explicit executable (`RAINY_PWSH_PATH`), used when it exists.
 * @param options.component `pwsh.exe` of the installed Windows basic component.
 * @param options.env Environment whose ProgramFiles, PATH and SystemRoot are consulted.
 * @returns The executable and its edition.
 */
export function resolvePowerShell(options: { configured?: string | undefined; component?: string | undefined; env?: NodeJS.ProcessEnv } = {}): PowerShellChoice {
  const env = options.env ?? process.env
  const candidates = [options.configured, options.component].filter((value): value is string => value !== undefined && value !== '')
  candidates.push(join(env.ProgramFiles ?? 'C:\\Program Files', 'PowerShell', '7', 'pwsh.exe'))
  for (const entry of pathValue(env).split(';')) {
    const directory = entry.trim().replace(/^"|"$/g, '')
    if (directory !== '') candidates.push(join(directory, 'pwsh.exe'))
  }
  const found = candidates.find(spawnable)
  if (found !== undefined) return { path: found, edition: basename(found).toLowerCase() === 'powershell.exe' ? 'desktop' : 'core' }
  return { path: join(env.SystemRoot ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe'), edition: 'desktop' }
}

/** Exit facts of a finished process. */
export interface ProcessOutcome {
  exitCode: number | null
  signal: NodeJS.Signals | null
}

/** Request for {@link spawnProcess}. */
export interface ProcessSpec {
  argv: readonly string[]
  cwd: string
  env?: NodeJS.ProcessEnv | undefined
  stdin?: 'pipe' | 'ignore' | undefined
  /** Wait between the polite and the forced stop. */
  graceMs?: number | undefined
  /** Aborting stops the whole process tree. */
  signal?: AbortSignal | undefined
}

/** A running child process. */
export interface ProcessHandle {
  readonly pid: number | undefined
  readonly stdin: Writable | undefined
  readonly stdout: Readable
  readonly stderr: Readable
  /** Settles when the direct child exits; rejects when it could not start. */
  readonly done: Promise<ProcessOutcome>
  /** Stop the whole process tree: a polite signal first, then a forced kill after the grace period. */
  terminate(): void
  /**
   * Wait until the tree stop finished.
   * @param signal Abandons the wait.
   * @returns Whether the process is known to have exited.
   */
  waitForExit(signal?: AbortSignal): Promise<boolean>
}

const DEFAULT_GRACE_MS = 3000

function windowsKillTree(pid: number, force: boolean): Promise<void> {
  return new Promise((resolve) => {
    const child = spawnChild('taskkill.exe', ['/PID', String(pid), '/T', ...(force ? ['/F'] : [])], { windowsHide: true, stdio: 'ignore' })
    child.on('error', () => { resolve() })
    child.on('exit', () => { resolve() })
  })
}

function posixKillGroup(pid: number, signal: NodeJS.Signals): void {
  try { process.kill(-pid, signal) } catch (_error) {
    try { process.kill(pid, signal) } catch (_ignored) { /* The process already exited. */ }
  }
}

/**
 * Stop a process and everything it started.
 * @param pid Root process id. On Linux it must lead its own process group.
 * @param graceMs Wait between the polite and the forced stop.
 * @param exited Resolves when the root process exits.
 * @returns Completion after the forced stop was sent or the process exited in time.
 */
export async function killTree(pid: number, graceMs: number, exited: Promise<unknown>): Promise<void> {
  if (process.platform === 'win32') {
    // Console programs ignore the polite request, so the forced stop follows after the grace period either way.
    await windowsKillTree(pid, false)
    const timer = new Promise<void>(resolve => setTimeout(resolve, graceMs).unref())
    await Promise.race([exited, timer])
    await windowsKillTree(pid, true)
    return
  }
  posixKillGroup(pid, 'SIGTERM')
  const timer = new Promise<'timeout'>(resolve => setTimeout(() => { resolve('timeout') }, graceMs).unref())
  await Promise.race([exited, timer])
  posixKillGroup(pid, 'SIGKILL')
}

/**
 * Start a program without a shell. On Linux the child leads a new process group so the whole tree can be stopped.
 * @param spec Program, directory, environment and stop behaviour.
 * @returns The running process.
 */
export function spawnProcess(spec: ProcessSpec): ProcessHandle {
  const [command, ...args] = spec.argv
  if (command === undefined) throw new Error('spawnProcess needs a program')
  const graceMs = spec.graceMs ?? DEFAULT_GRACE_MS
  const child: ChildProcess = spawnChild(command, args, {
    cwd: spec.cwd,
    env: spec.env ?? scrubbedEnv(),
    stdio: [spec.stdin ?? 'pipe', 'pipe', 'pipe'],
    detached: process.platform !== 'win32',
    windowsHide: true,
  })
  const exited = new Promise<ProcessOutcome>((resolve, reject) => {
    child.once('error', reject)
    child.once('exit', (exitCode, signal) => { resolve({ exitCode, signal }) })
  })
  let stopping: Promise<void> | undefined
  const terminate = (): void => {
    if (stopping !== undefined || child.pid === undefined) return
    stopping = killTree(child.pid, graceMs, exited.catch(() => undefined))
  }
  const onAbort = (): void => { terminate() }
  if (spec.signal !== undefined) {
    if (spec.signal.aborted) queueMicrotask(terminate)
    else spec.signal.addEventListener('abort', onAbort, { once: true })
  }
  void exited.finally(() => { spec.signal?.removeEventListener('abort', onAbort) }).catch(() => undefined)
  return {
    pid: child.pid,
    stdin: child.stdin ?? undefined,
    stdout: child.stdout!,
    stderr: child.stderr!,
    done: exited,
    terminate,
    async waitForExit(signal?: AbortSignal): Promise<boolean> {
      const abandoned = new Promise<false>((resolve) => {
        if (signal?.aborted) resolve(false)
        signal?.addEventListener('abort', () => { resolve(false) }, { once: true })
      })
      return Promise.race([exited.then(async () => { await stopping; return true }, () => true), abandoned])
    },
  }
}

/**
 * Run a program to completion and collect its output.
 * @param spec Program request; stdin is ignored.
 * @param limitBytes Output kept per stream; later bytes are dropped.
 * @returns Exit facts with decoded stdout and stderr.
 */
export async function runProcess(spec: ProcessSpec, limitBytes = 4 * 1024 * 1024): Promise<ProcessOutcome & { stdout: string; stderr: string }> {
  const handle = spawnProcess({ ...spec, stdin: 'ignore' })
  const collect = async (stream: Readable): Promise<string> => {
    const chunks: Buffer[] = []
    let size = 0
    for await (const chunk of stream) {
      const buffer = chunk as Buffer
      if (size < limitBytes) chunks.push(buffer.subarray(0, limitBytes - size))
      size += buffer.length
    }
    return Buffer.concat(chunks).toString('utf8')
  }
  const [stdout, stderr, outcome] = await Promise.all([collect(handle.stdout), collect(handle.stderr), handle.done])
  return { ...outcome, stdout, stderr }
}

/** Request for {@link spawnTerminal}. */
export interface TerminalSpec {
  argv: readonly string[]
  cwd: string
  env?: Record<string, string> | undefined
  cols: number
  rows: number
  terminalType?: string | undefined
  graceMs?: number | undefined
  signal?: AbortSignal | undefined
}

/** A program running in a pseudo-terminal. */
export interface TerminalHandle {
  readonly pid: number
  /** Terminal output as UTF-8 text chunks. */
  readonly output: Readable
  readonly done: Promise<ProcessOutcome>
  write(data: string): Promise<void>
  resize(cols: number, rows: number): Promise<void>
  /** Stop the terminal's process tree and wait for it. */
  terminate(): Promise<void>
}

let ptyModule: typeof import('node-pty') | undefined
/**
 * What node-pty throws when a resize reaches a terminal whose program has exited but whose onExit has not fired:
 * the Windows agent's own check, and the closed descriptor on Linux. node-pty is pinned, so the texts are stable.
 */
const PTY_CLOSED: ReadonlySet<string> = new Set(['Cannot resize a pty that has already exited', 'ioctl(2) failed, EBADF'])

function loadPty(): typeof import('node-pty') {
  ptyModule ??= createRequire(import.meta.url)('node-pty') as typeof import('node-pty')
  return ptyModule
}

/**
 * Start a program in a pseudo-terminal (ConPTY on Windows).
 * @param spec Program, directory, environment and size.
 * @returns The running terminal.
 */
export async function spawnTerminal(spec: TerminalSpec): Promise<TerminalHandle> {
  spec.signal?.throwIfAborted()
  const [command, ...args] = spec.argv
  if (command === undefined) throw new Error('spawnTerminal needs a program')
  const pty: IPty = loadPty().spawn(command, args, {
    name: spec.terminalType ?? 'xterm-256color',
    cols: spec.cols,
    rows: spec.rows,
    cwd: spec.cwd,
    env: spec.env ?? scrubbedEnv(),
    ...(process.platform === 'win32' ? { useConpty: true } : {}),
  })
  const output = new PassThrough({ encoding: 'utf8' })
  const graceMs = spec.graceMs ?? DEFAULT_GRACE_MS
  let exited = false
  // On Windows node-pty queues a resize until the terminal's first output and runs it from an event handler,
  // where a program that already exited throws uncaught. Until that output, a resize only records the size.
  let ready = process.platform !== 'win32'
  let pendingSize: { cols: number; rows: number } | undefined
  const applySize = (cols: number, rows: number): void => {
    // A layout change can arrive after the program ended, before onExit fires.
    if (exited) return
    try { pty.resize(cols, rows) } catch (error) {
      if (!(error instanceof Error && PTY_CLOSED.has(error.message))) throw error
    }
  }
  pty.onData((data) => {
    output.write(data)
    if (ready) return
    ready = true
    // node-pty marks itself ready after this listener on the same output, so the next turn resizes immediately.
    setImmediate(() => {
      if (pendingSize !== undefined) applySize(pendingSize.cols, pendingSize.rows)
      pendingSize = undefined
    })
  })
  const done = new Promise<ProcessOutcome>((resolve) => {
    pty.onExit(({ exitCode, signal }) => {
      exited = true
      output.end()
      resolve({ exitCode, signal: signal ? (`SIG${signal}` as NodeJS.Signals) : null })
    })
  })
  let stopping: Promise<void> | undefined
  const terminate = (): Promise<void> => {
    stopping ??= (async () => {
      if (process.platform === 'win32') {
        await windowsKillTree(pty.pid, true)
        try { pty.kill() } catch (_error) { /* The terminal already closed. */ }
      } else await killTree(pty.pid, graceMs, done)
      await Promise.race([done, new Promise(resolve => setTimeout(resolve, graceMs).unref())])
    })()
    return stopping
  }
  spec.signal?.addEventListener('abort', () => { void terminate() }, { once: true })
  return {
    pid: pty.pid,
    output,
    done,
    write(data: string): Promise<void> { pty.write(data); return Promise.resolve() },
    resize(cols: number, rows: number): Promise<void> {
      if (ready && pendingSize === undefined) applySize(cols, rows)
      else pendingSize = { cols, rows }
      return Promise.resolve()
    },
    terminate,
  }
}
