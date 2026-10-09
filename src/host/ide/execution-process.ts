/** Process and PTY ownership shared by human runs, terminals and launch-only debug sessions. */
import { isAbsolute } from 'node:path'
import type { Readable, Writable } from 'node:stream'
import { StringDecoder } from 'node:string_decoder'
import { userInfo } from 'node:os'
import type { IdeCommandSpec } from '../../shared/ide-execution-protocol.ts'
import {
  ExecutableNotFoundError, resolveExecutable, scrubbedEnv, spawnProcess, spawnTerminal,
  type ProcessHandle, type ProcessOutcome, type TerminalHandle,
} from '../process.ts'

/** Text channels emitted by owned processes. */
export type IdeOutputStream = 'stdout' | 'stderr' | 'terminal' | 'adapter'

/** A program started without a shell; `environment` is layered over the Host's scrubbed environment. */
export interface IdeSpawnSpec {
  readonly argv: readonly string[]
  readonly cwd: string
  readonly environment?: Readonly<Record<string, string>> | undefined
  readonly stdin?: 'pipe' | 'ignore' | undefined
  readonly graceMs?: number | undefined
  readonly signal?: AbortSignal | undefined
}

/** A program started in a pseudo-terminal with `xterm-256color`. */
export interface IdeTerminalSpawnSpec {
  readonly argv: readonly string[]
  readonly cwd: string
  readonly environment?: Readonly<Record<string, string>> | undefined
  readonly cols: number
  readonly rows: number
  readonly graceMs?: number | undefined
  readonly signal?: AbortSignal | undefined
}

/** Execution world of the Host process. */
export interface IdeTerminalEnvironment {
  readonly platform: 'windows' | 'posix'
  /** `ComSpec` on Windows; `SHELL` or the account's login shell elsewhere. */
  readonly defaultShell?: string | undefined
}

/** Process operations consumed by the IDE; tests substitute controlled handles. */
export interface IdeSubprocess {
  /** @param spec Program, directory and environment overlay. @returns The running process. */
  spawn(spec: IdeSpawnSpec): ProcessHandle
  /** @param spec Program, directory, overlay and size. @returns The running terminal. */
  spawnTerminal(spec: IdeTerminalSpawnSpec): Promise<TerminalHandle>
  /**
   * Find an executable.
   * @param command Absolute path or bare program name looked up on PATH.
   * @param environment Overlay whose PATH is searched.
   * @returns The absolute executable path.
   */
  resolveExecutable(command: string, environment?: Readonly<Record<string, string>>): Promise<string>
  /** @returns The Host's platform family and default shell. */
  terminalEnvironment(): Promise<IdeTerminalEnvironment>
}

/**
 * Layer an explicit overlay over the Host's environment without credential-shaped names.
 * @param overlay Variables chosen by the IDE (runtime PATH, run configuration).
 * @returns A complete child environment; on Windows an overlay name replaces any differently cased inherited name.
 */
export function ideChildEnvironment(overlay: Readonly<Record<string, string>> = {}): Record<string, string> {
  const environment = scrubbedEnv()
  for (const [name, value] of Object.entries(overlay)) {
    if (process.platform === 'win32') {
      for (const inherited of Object.keys(environment)) if (inherited.toUpperCase() === name.toUpperCase()) delete environment[inherited]
    }
    environment[name] = value
  }
  return environment
}

function defaultShell(): string | undefined {
  if (process.platform === 'win32') return process.env.ComSpec || undefined
  if (process.env.SHELL) return process.env.SHELL
  try { return userInfo().shell ?? undefined } catch (_noAccount) { return undefined }
}

/** Local processes through `src/host/process.ts`. */
export const localIdeSubprocess: IdeSubprocess = {
  spawn: spec => spawnProcess({
    argv: spec.argv, cwd: spec.cwd, env: ideChildEnvironment(spec.environment), stdin: spec.stdin ?? 'pipe',
    graceMs: spec.graceMs, signal: spec.signal,
  }),
  spawnTerminal: spec => spawnTerminal({
    argv: spec.argv, cwd: spec.cwd, env: ideChildEnvironment(spec.environment), cols: spec.cols, rows: spec.rows,
    terminalType: 'xterm-256color', graceMs: spec.graceMs, signal: spec.signal,
  }),
  resolveExecutable(command, environment) {
    if (command === '') return Promise.reject(new Error('An executable name is required.'))
    if (!isAbsolute(command) && (command.includes('/') || (process.platform === 'win32' && command.includes('\\')))) {
      return Promise.reject(new Error(`The command ${JSON.stringify(command)} is a relative path; use an absolute path or a program name on PATH.`))
    }
    const found = resolveExecutable(command, ideChildEnvironment(environment))
    return found === undefined ? Promise.reject(new ExecutableNotFoundError(command)) : Promise.resolve(found)
  },
  terminalEnvironment: () => Promise.resolve({ platform: process.platform === 'win32' ? 'windows' : 'posix', defaultShell: defaultShell() }),
}

/**
 * Write input and observe callback failures.
 * @param stream Owned writable pipe.
 * @param data Literal user input.
 * @returns Write completion.
 */
export function writeIdeInput(stream: Writable | undefined, data: string): Promise<void> {
  if (!stream || stream.destroyed) return Promise.reject(new Error('This operation has no open input pipe.'))
  return new Promise<void>((resolve, reject) => {
    stream.write(data, (error) => {
      if (error) reject(error)
      else resolve()
    })
  })
}

/** One admission and teardown owner for every process of an IDE operation. */
export class IdeProcessOwner {
  private readonly controller = new AbortController()
  private readonly processes = new Set<ProcessHandle>()
  private readonly terminals = new Set<TerminalHandle>()
  private readonly allocations = new Set<Promise<TerminalHandle>>()
  private readonly pumps = new Set<Promise<void>>()
  private state: 'active' | 'stopping' | 'closed' = 'active'
  private closing?: Promise<void>
  private streamFailure?: Error
  /** Cancels process allocation and requests process-tree termination. */
  get signal(): AbortSignal {
    return this.controller.signal
  }

  /**
   * @param subprocess Process provider.
   * @param graceMs Wait between the polite and the forced stop.
   * @param output Operation-owned output sink.
   */
  constructor(
    private readonly subprocess: IdeSubprocess,
    private readonly graceMs: number,
    private readonly output: (stream: IdeOutputStream, text: string) => void,
  ) {}

  /**
   * Spawn a process; the owner keeps it until its whole tree has stopped.
   * @param spec Resolved argv, directory and environment.
   * @param stdin Input disposition.
   * @returns Its live handle.
   */
  spawn(spec: IdeCommandSpec, stdin: 'pipe' | 'ignore' = 'pipe'): ProcessHandle {
    this.requireActive()
    const handle = this.subprocess.spawn({
      argv: spec.argv, cwd: spec.cwd, environment: spec.environment, signal: this.signal, graceMs: this.graceMs, stdin,
    })
    this.processes.add(handle)
    void handle.done.catch((_startFailure: unknown) => { /* The operation or teardown observes the same rejection. */ })
    return handle
  }

  /**
   * Allocate a terminal with no shell interpolation of argv.
   * @param spec Resolved program.
   * @param cols Column count.
   * @param rows Row count.
   * @returns The owned terminal.
   */
  async terminal(spec: IdeCommandSpec, cols: number, rows: number): Promise<TerminalHandle> {
    this.requireActive()
    const allocation = this.subprocess.spawnTerminal({
      argv: spec.argv, cwd: spec.cwd, environment: { ...spec.environment }, cols, rows, graceMs: this.graceMs, signal: this.signal,
    })
    this.allocations.add(allocation)
    try {
      const terminal = await allocation
      this.terminals.add(terminal)
      void terminal.done.catch((_exitFailure: unknown) => { /* The operation or teardown observes the same rejection. */ })
      if (this.state !== 'active') {
        await terminal.terminate()
        throw new Error('The operation was stopped while its terminal was being created.')
      }
      return terminal
    } finally {
      this.allocations.delete(allocation)
    }
  }

  /**
   * Drain a stream with complete UTF-8 decoding across chunks.
   * @param stream Owned stream.
   * @param channel Presentation channel.
   * @returns Drain completion.
   */
  pump(stream: Readable | undefined, channel: IdeOutputStream): Promise<void> {
    if (!stream) return Promise.resolve()
    const task = (async () => {
      const decoder = new StringDecoder('utf8')
      try {
        for await (const chunk of stream) {
          if (!Buffer.isBuffer(chunk) && typeof chunk !== 'string') throw new Error('The process returned a non-byte output stream.')
          const text = decoder.write(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk))
          if (text && this.state !== 'closed') this.output(channel, text)
        }
        const tail = decoder.end()
        if (tail && this.state !== 'closed') this.output(channel, tail)
      } catch (error) {
        if (this.state === 'active') {
          this.streamFailure = error instanceof Error ? error : new Error(String(error))
          this.controller.abort(this.streamFailure)
        }
      }
    })()
    this.pumps.add(task)
    void task.finally(() => { this.pumps.delete(task) })
    return task
  }

  /**
   * Run one build command and wait for its exit, its output and its process tree.
   * @param spec Resolved build command.
   * @returns Exit facts.
   */
  async command(spec: IdeCommandSpec): Promise<ProcessOutcome> {
    const handle = this.spawn(spec, 'ignore')
    return this.waitProcess(handle, [this.pump(handle.stdout, 'stdout'), this.pump(handle.stderr, 'stderr')])
  }

  /**
   * Join a process, its tree and its output before publishing completion.
   * @param handle Owned process.
   * @param output Its drain promises.
   * @returns The direct process exit facts.
   */
  async waitProcess(handle: ProcessHandle, output: readonly Promise<void>[]): Promise<ProcessOutcome> {
    try {
      const outcome = await handle.done
      handle.terminate()
      await handle.waitForExit()
      await Promise.all(output)
      if (this.streamFailure) throw this.streamFailure
      return outcome
    } finally {
      handle.terminate()
      await handle.waitForExit()
      this.processes.delete(handle)
    }
  }

  /**
   * Join a terminal and every process it started.
   * @param terminal Owned terminal.
   * @param output Its drain promise.
   * @returns The direct process exit facts.
   */
  async waitTerminal(terminal: TerminalHandle, output: Promise<void>): Promise<ProcessOutcome> {
    try {
      const outcome = await terminal.done
      await terminal.terminate()
      await output
      if (this.streamFailure) throw this.streamFailure
      return outcome
    } finally {
      await terminal.terminate()
      this.terminals.delete(terminal)
    }
  }

  /**
   * Stop admission, cancel pending allocation, terminate owned processes and await all drains.
   * @returns Completion shared by repeated calls.
   */
  close(): Promise<void> {
    if (this.closing) return this.closing
    this.state = 'stopping'
    this.controller.abort(new Error('The IDE operation was stopped.'))
    for (const process of this.processes) process.terminate()
    this.closing = (async () => {
      const failures: unknown[] = []
      const allocations = await Promise.allSettled([...this.allocations])
      for (const result of allocations) if (result.status === 'fulfilled') this.terminals.add(result.value)
      const settled = await Promise.allSettled([
        ...[...this.processes].map(async (process) => {
          process.terminate()
          await Promise.allSettled([process.done])
          await process.waitForExit()
        }),
        ...[...this.terminals].map(terminal => terminal.terminate()),
      ])
      for (const result of settled) if (result.status === 'rejected') failures.push(result.reason)
      await Promise.all([...this.pumps])
      this.processes.clear()
      this.terminals.clear()
      this.state = 'closed'
      if (failures.length) throw new AggregateError(failures, 'The IDE could not confirm process cleanup.')
    })()
    return this.closing
  }

  private requireActive(): void {
    if (this.state !== 'active') throw new Error('The IDE operation is stopping or closed.')
    this.signal.throwIfAborted()
  }
}
