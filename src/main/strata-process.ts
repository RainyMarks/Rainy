/** Hidden, owned Strata subprocesses whose exit is awaited before replacing their configuration. */
import { execFile, spawn } from 'node:child_process'
import { closeSync, openSync } from 'node:fs'

/** Settled child state; spawn failures are returned without leaking an unhandled rejection. */
export interface StrataProcessExit {
  readonly code: number | null
  readonly signal: NodeJS.Signals | null
  readonly error?: Error
}

/** A process handle is retained only for a child this carrier created. */
export interface StrataProcess {
  readonly pid: number | undefined
  readonly exited: Promise<StrataProcessExit>
  readonly finished: boolean
  /** @returns only when this child and its owned process tree have exited. */
  stop(): Promise<void>
}

/** Fully resolved child invocation; neither a shell nor a user-provided command is used. */
export interface StrataProcessSpec {
  readonly executable: string
  readonly args: readonly string[]
  readonly cwd: string
  readonly environment: Readonly<Record<string, string>>
  readonly logPath: string
}

/**
 * Spawn one bundled interpreter and keep logs out of status and IPC responses.
 * @param spec - explicit executable, arguments, private log, and scrubbed environment.
 * @returns an owned handle that never attaches to an existing process by PID.
 */
export function spawnStrataProcess(spec: StrataProcessSpec): StrataProcess {
  const log = openSync(spec.logPath, 'a', 0o600)
  let child
  try {
    child = spawn(spec.executable, [...spec.args], { cwd: spec.cwd, env: { ...spec.environment },
      windowsHide: true, stdio: ['ignore', log, log] })
  } finally { closeSync(log) }
  let failure: Error | undefined
  let finished = false
  let stopping: Promise<void> | undefined
  const exited = new Promise<StrataProcessExit>((resolve) => {
    child.once('error', (error) => { failure = error })
    child.once('close', (code, signal) => { finished = true; resolve({ code, signal, ...(failure ? { error: failure } : {}) }) })
  })
  return {
    pid: child.pid,
    exited,
    get finished() { return finished },
    stop() {
      stopping ??= (async () => {
        if (finished) return
        if (process.platform === 'win32' && child.pid !== undefined) {
          await new Promise<void>((resolve) => {
            execFile('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, timeout: 10000 }, () => {
              if (!finished) child.kill()
              resolve()
            })
          })
        } else child.kill('SIGTERM')
        const timer = setTimeout(() => { if (!finished) child.kill('SIGKILL') }, 5000)
        try { await exited } finally { clearTimeout(timer) }
      })()
      return stopping
    },
  }
}
