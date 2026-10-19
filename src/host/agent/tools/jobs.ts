/**
 * Commands that keep running after their tool call returns: development servers, watchers, builds.
 * One registry per chat; its jobs are stopped when the chat closes or the Host stops.
 */
import type { BackgroundCommand } from '../../../shared/rpc.ts'
import { consoleText, spawnProcess, type ProcessHandle } from '../../process.ts'

/** Jobs one chat may have running at once; a new one past this is refused rather than silently queued. */
const MAX_JOBS = 8
/** Output kept per job until the model reads it; older output is dropped and the drop is reported. */
const BUFFER_BYTES = 256 * 1024

/** What the model and the page see of a background command. */
export type BackgroundJob = BackgroundCommand

interface Entry {
  job: BackgroundJob
  handle: ProcessHandle
  chunks: Buffer[]
  bytes: number
  /** Output bytes dropped because the buffer was full; reported once with the next read. */
  dropped: number
}

/** Background commands of one chat. */
export class BackgroundJobs {
  private readonly entries = new Map<string, Entry>()
  private next = 1

  /** @param onChange Called when a job starts, exits or is stopped, so the page can follow. */
  constructor(private readonly onChange: () => void = () => undefined) {}

  /**
   * Start a command that keeps running.
   * @param spec Program and environment, as the shell tool builds them.
   * @returns The new job.
   */
  start(spec: { argv: string[]; cwd: string; env: Record<string, string>; command: string; description: string }): BackgroundJob {
    const live = [...this.entries.values()].filter(entry => entry.job.state === 'running').length
    if (live >= MAX_JOBS) {
      throw new Error(`too many background commands are running (${live}); stop one with stop_command before starting another`)
    }
    const id = `cmd-${this.next++}`
    const handle = spawnProcess({ argv: spec.argv, cwd: spec.cwd, env: spec.env, stdin: 'ignore', graceMs: 3000 })
    const entry: Entry = {
      job: { id, command: spec.command, description: spec.description, cwd: spec.cwd, startedAt: Date.now(), state: 'running', exitCode: null, signal: null, pending: 0 },
      handle, chunks: [], bytes: 0, dropped: 0,
    }
    this.entries.set(id, entry)
    const collect = (stream: NodeJS.ReadableStream): void => {
      stream.on('data', (chunk: Buffer) => { this.push(entry, chunk) })
      stream.on('error', () => undefined)
    }
    collect(handle.stdout)
    collect(handle.stderr)
    void handle.done.then(
      (outcome) => { entry.job.state = 'exited'; entry.job.exitCode = outcome.exitCode; entry.job.signal = outcome.signal; this.onChange() },
      (error: unknown) => {
        entry.job.state = 'exited'
        this.push(entry, Buffer.from(`\n[could not start: ${error instanceof Error ? error.message : String(error)}]\n`, 'utf8'))
        this.onChange()
      },
    )
    this.onChange()
    return { ...entry.job }
  }

  private push(entry: Entry, chunk: Buffer): void {
    entry.chunks.push(chunk)
    entry.bytes += chunk.length
    while (entry.bytes > BUFFER_BYTES && entry.chunks.length > 1) {
      const dropped = entry.chunks.shift()!
      entry.bytes -= dropped.length
      entry.dropped += dropped.length
    }
    entry.job.pending = entry.bytes
  }

  private entry(id: string): Entry {
    const entry = this.entries.get(id.trim())
    if (entry === undefined) {
      const known = [...this.entries.keys()]
      throw new Error(`unknown background command "${id}"${known.length === 0 ? '; none is running' : `; running or finished: ${known.join(', ')}`}`)
    }
    return entry
  }

  /**
   * Take the output a job produced since the last read.
   * @param id Job id.
   * @returns The job and its new output.
   */
  read(id: string): { job: BackgroundJob; text: string } {
    const entry = this.entry(id)
    const buffer = Buffer.concat(entry.chunks)
    entry.chunks = []
    entry.bytes = 0
    entry.job.pending = 0
    const dropped = entry.dropped
    entry.dropped = 0
    const text = (dropped > 0 ? `[${dropped} earlier bytes dropped; the command produced more output than is kept]\n` : '') + consoleText(buffer)
    // A finished job is forgotten once its last output has been read.
    if (entry.job.state === 'exited') this.entries.delete(entry.job.id)
    return { job: { ...entry.job }, text }
  }

  /**
   * Drop a finished job from the list without reading it; its unread output is lost.
   * @param id Job id.
   */
  forget(id: string): void {
    const entry = this.entry(id)
    if (entry.job.state === 'running') throw new Error(`background command "${entry.job.id}" is still running; stop it first`)
    this.entries.delete(entry.job.id)
    this.onChange()
  }

  /** @returns Every job, newest last. */
  list(): BackgroundJob[] {
    return [...this.entries.values()].map(entry => ({ ...entry.job }))
  }

  /**
   * Stop a running job and wait for the process to go; its unread output stays readable.
   * @param id Job id.
   * @returns The job.
   */
  async stop(id: string): Promise<BackgroundJob> {
    const entry = this.entry(id)
    entry.handle.terminate()
    await entry.handle.waitForExit(AbortSignal.timeout(5000))
    entry.job.state = 'exited'
    this.onChange()
    return { ...entry.job }
  }

  /** Stop every job and wait for the processes to go. */
  async close(): Promise<void> {
    const entries = [...this.entries.values()]
    this.entries.clear()
    for (const entry of entries) entry.handle.terminate()
    await Promise.allSettled(entries.map(entry => entry.handle.waitForExit(AbortSignal.timeout(5000))))
    this.onChange()
  }
}
