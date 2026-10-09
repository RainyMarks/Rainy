/** Managed process and PTY ownership shared by human runs and launch-only debug sessions. */
import type { Readable } from 'node:stream'
import { StringDecoder } from 'node:string_decoder'
import type { Writable } from 'node:stream'
import type { SubprocessHandle, SubprocessOutcome, SubprocessRuntime, SubprocessTerminalHandle } from '@deepseek-ai/dsh-subprocess'
import type { IdeCommandSpec } from '@deepseek-ai/dsh-client-ui-rainy/ide-execution-protocol'

/** Provider operations consumed by the IDE process owner. */
export type IdeSubprocess = Pick<SubprocessRuntime, 'spawn' | 'spawnTerminal' | 'resolveExecutable' | 'terminalEnvironment'>
/** Text channels emitted by owned processes. */
export type IdeOutputStream = 'stdout' | 'stderr' | 'terminal' | 'adapter'

/**
 * Write input and observe callback failures.
 * @param stream - owned writable pipe.
 * @param data - literal user input.
 * @returns write completion.
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

/** One admission and teardown owner for every process associated with an IDE operation. */
export class IdeProcessOwner {
  private readonly controller = new AbortController()
  private readonly processes = new Set<SubprocessHandle>()
  private readonly terminals = new Set<SubprocessTerminalHandle>()
  private readonly allocations = new Set<Promise<SubprocessTerminalHandle>>()
  private readonly pumps = new Set<Promise<void>>()
  private state: 'active' | 'stopping' | 'closed' = 'active'
  private closing?: Promise<void>
  private streamFailure?: Error
  /** Cancels process allocation and requests managed-range termination. */
  get signal(): AbortSignal {
    return this.controller.signal
  }

  /**
   * @param subprocess - shared native managed process provider.
   * @param graceMs - provider cleanup grace.
   * @param output - operation-owned output sink.
   */
  constructor(
    private readonly subprocess: IdeSubprocess,
    private readonly graceMs: number,
    private readonly output: (stream: IdeOutputStream, text: string) => void,
  ) {}

  /**
   * Spawn a raw process; the owner retains it through managed-range quiescence.
   * @param spec - resolved argv/cwd/environment.
   * @param stdin - required input disposition.
   * @returns its live handle.
   */
  spawn(spec: IdeCommandSpec, stdin: 'pipe' | 'ignore' = 'pipe'): SubprocessHandle {
    this.requireActive()
    const handle = this.subprocess.spawn({
      argv: spec.argv,
      cwd: spec.cwd,
      env: spec.environment,
      signal: this.signal,
      graceMs: this.graceMs,
      stdio: { stdin, stdout: 'pipe', stderr: 'pipe' },
    })
    this.processes.add(handle)
    void handle.done.catch(() => {
      /* The operation or teardown observes the same rejection. */
    })
    return handle
  }

  /**
   * Allocate a terminal with no shell interpolation of argv.
   * @param spec - resolved program.
   * @param cols - column count.
   * @param rows - row count.
   * @returns the owned terminal.
   */
  async terminal(spec: IdeCommandSpec, cols: number, rows: number): Promise<SubprocessTerminalHandle> {
    this.requireActive()
    const allocation = this.subprocess.spawnTerminal({
      argv: spec.argv,
      cwd: spec.cwd,
      env: { ...spec.environment },
      cols,
      rows,
      terminalType: 'xterm-256color',
      graceMs: this.graceMs,
      signal: this.signal,
    })
    this.allocations.add(allocation)
    try {
      const terminal = await allocation
      this.terminals.add(terminal)
      void terminal.done.catch(() => {
        /* The operation or teardown observes the same rejection. */
      })
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
   * @param stream - owned stream.
   * @param channel - presentation channel.
   * @returns drain completion.
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
    void task.finally(() => {
      this.pumps.delete(task)
    })
    return task
  }

  /**
   * Run one build command and await direct exit, output, and the owned process range.
   * @param spec - resolved build command.
   * @returns independent exit facts.
   */
  async command(spec: IdeCommandSpec): Promise<SubprocessOutcome> {
    const handle = this.spawn(spec, 'ignore')
    return this.waitProcess(handle, [this.pump(handle.stdout, 'stdout'), this.pump(handle.stderr, 'stderr')])
  }

  /**
   * Join a raw command and its output before publishing completion.
   * @param handle - owned process.
   * @param output - its drain promises.
   * @returns the direct process exit facts.
   */
  async waitProcess(handle: SubprocessHandle, output: readonly Promise<void>[]): Promise<SubprocessOutcome> {
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
   * Join a terminal and all its session members.
   * @param terminal - owned terminal.
   * @param output - its drain promise.
   * @returns the direct process exit facts.
   */
  async waitTerminal(terminal: SubprocessTerminalHandle, output: Promise<void>): Promise<SubprocessOutcome> {
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
   * Stop admission, cancel pending allocation, terminate owned ranges, and await all drains.
   * @returns quiescent completion shared by repeated calls.
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
