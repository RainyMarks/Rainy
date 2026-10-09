/** `bash` (WSL/Linux) and `pwsh` (Windows): one fresh shell per command. */
import { createWriteStream, existsSync, type WriteStream } from 'node:fs'
import { mkdir } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { randomBytes } from 'node:crypto'
import { Type, type Static } from '@earendil-works/pi-ai'
import type { AgentTool, AgentToolUpdateCallback } from '@earendil-works/pi-agent-core'
import { resolveExecutable, scrubbedEnv, spawnProcess } from '../../process.ts'
import { limitOutput, textResult, type ToolContext } from './common.ts'

/** Timeout when the model does not set one. */
export const DEFAULT_TIMEOUT_MS = 60_000
/** Longest timeout a command may request. */
export const MAX_TIMEOUT_MS = 600_000
/** Bytes of each stream kept in memory (the tail). */
const TAIL_BYTES = 64_000
/** Bytes of each stream saved to the full-output file. */
const SPILL_BYTES = 64 * 1024 * 1024
const ENCODING_PREAMBLE = '[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); $OutputEncoding = [System.Text.UTF8Encoding]::new($false); '

/** Command facts for the UI's terminal card. */
export interface ShellDetails {
  command: string
  description: string
  cwd: string
  exitCode: number | null
  signal: string | null
  timedOut: boolean
  durationMs: number
}

const parameters = (shell: 'bash' | 'pwsh') => Type.Object({
  command: Type.String({ description: shell === 'bash' ? 'The bash command to execute.' : 'The PowerShell command to execute.' }),
  description: Type.String({
    description: 'Clear, concise description of what this command does in active voice, '
      + '5-10 words (shown in the UI). Examples: "ls" → "List files in current directory"; '
      + '"git status" → "Show working tree status"; "npm install" → "Install package dependencies".',
  }),
  timeoutMs: Type.Optional(Type.Number({ description: 'Timeout in milliseconds. The executor applies its configured default and cap, and kills the command on expiry.' })),
  workdir: Type.Optional(Type.String({ description: 'Working directory for this command. Defaults to the session workspace; a relative path is resolved against it.' })),
})

/** Collects one output stream: a bounded tail in memory and the full text in a file once it grows large. */
class StreamCollector {
  private chunks: Buffer[] = []
  private size = 0
  private total = 0
  private file: WriteStream | undefined
  private filePath: string | undefined

  constructor(private readonly spill: () => Promise<string>) {}

  async push(chunk: Buffer): Promise<void> {
    this.total += chunk.length
    if (this.file === undefined && this.total > TAIL_BYTES) {
      this.filePath = await this.spill()
      this.file = createWriteStream(this.filePath, { mode: 0o600 })
      for (const earlier of this.chunks) this.file.write(earlier)
    }
    if (this.file !== undefined && this.total - chunk.length < SPILL_BYTES) this.file.write(chunk.subarray(0, Math.max(0, SPILL_BYTES - (this.total - chunk.length))))
    this.chunks.push(chunk)
    this.size += chunk.length
    while (this.size - (this.chunks[0]?.length ?? 0) >= TAIL_BYTES && this.chunks.length > 1) this.size -= this.chunks.shift()!.length
  }

  async close(): Promise<void> {
    const file = this.file
    if (file !== undefined) await new Promise<void>(done => { file.end(done) })
  }

  text(): string {
    let buffer = Buffer.concat(this.chunks)
    if (buffer.length > TAIL_BYTES) buffer = buffer.subarray(buffer.length - TAIL_BYTES)
    const text = buffer.toString('utf8')
    return this.filePath === undefined ? text : `${text}\n[output truncated; full output: ${this.filePath}]`
  }
}

/**
 * Model-facing result text.
 * @param stdout Collected stdout text.
 * @param stderr Collected stderr text.
 * @param outcome Exit facts.
 * @returns Output (or `(no output)`), then timeout and exit markers on their own lines.
 */
export function renderShellResult(stdout: string, stderr: string, outcome: { exitCode: number | null; signal: string | null; timedOut: boolean; timeoutMs: number }): string {
  let body = stdout
  if (stderr.length > 0) {
    if (body.length > 0 && !body.endsWith('\n')) body += '\n'
    body += `[stderr]\n${stderr}`
  }
  if (body.length === 0) body = '(no output)'
  const markers: string[] = []
  if (outcome.timedOut) markers.push(`[timed out after ${outcome.timeoutMs}ms]`)
  if (outcome.signal !== null) markers.push(`[killed by signal: ${outcome.signal}]`)
  else if (outcome.exitCode !== 0) markers.push(`[exit code: ${outcome.exitCode ?? 'unknown'}]`)
  if (markers.length === 0) return body
  if (!body.endsWith('\n')) body += '\n'
  return body + markers.join('\n')
}

/**
 * Find PowerShell 7, falling back to Windows PowerShell.
 * @param configured Path shipped with the Windows Host.
 * @returns Executable path.
 */
export function pwshExecutable(configured: string | undefined): string {
  if (configured !== undefined && existsSync(configured)) return configured
  const programFiles = process.env.ProgramFiles ?? 'C:\\Program Files'
  const installed = join(programFiles, 'PowerShell', '7', 'pwsh.exe')
  if (existsSync(installed)) return installed
  return resolveExecutable('pwsh') ?? join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe')
}

/**
 * The shell tool for this platform.
 * @param context Chat tool context.
 * @param shell `bash` on Linux/WSL, `pwsh` on Windows.
 * @returns Tool definition.
 */
export function shellTool(context: ToolContext, shell: 'bash' | 'pwsh'): AgentTool<ReturnType<typeof parameters>, ShellDetails> {
  const schema = parameters(shell)
  return {
    name: shell,
    label: shell === 'bash' ? 'Bash' : 'PowerShell',
    description: shell === 'bash'
      ? 'Run a command in the project Bash shell. Use rg for search; inspect exit status.'
      : 'Run a command in the project PowerShell shell. Use rg for search; inspect exit status.',
    parameters: schema,
    executionMode: 'sequential',
    async execute(_id, params: Static<typeof schema>, signal, onUpdate?: AgentToolUpdateCallback<ShellDetails>) {
      if (params.command.trim() === '') throw new Error('invalid command: expected a non-empty string')
      if (params.description.trim() === '') throw new Error('invalid description: expected a non-empty string')
      const timeoutMs = Math.min(MAX_TIMEOUT_MS, Math.max(1, Math.floor(params.timeoutMs ?? DEFAULT_TIMEOUT_MS)))
      const cwd = params.workdir === undefined ? context.cwd : resolve(context.cwd, params.workdir)
      const runtime = context.runtimeEnvironment(cwd).environment
      const env = {
        ...scrubbedEnv(), NO_COLOR: '1', TERM: 'dumb', PAGER: 'cat', GIT_PAGER: 'cat', ...runtime, RAINY_SESSION_ID: context.sessionId,
      }
      const argv = shell === 'bash'
        ? ['bash', '-c', params.command]
        : [pwshExecutable(context.pwshPath), '-NoLogo', '-NoProfile', '-NonInteractive', '-Command', `${ENCODING_PREAMBLE}${params.command}`]
      const started = Date.now()
      const spill = async (stream: string): Promise<string> => {
        await mkdir(context.spillDir, { recursive: true, mode: 0o700 })
        return join(context.spillDir, `${randomBytes(6).toString('hex')}-${shell}-${stream}.txt`)
      }
      const stdout = new StreamCollector(() => spill('stdout'))
      const stderr = new StreamCollector(() => spill('stderr'))
      const handle = spawnProcess({ argv, cwd, env, stdin: 'ignore', graceMs: 3000, signal })
      let timedOut = false
      const timer = setTimeout(() => { timedOut = true; handle.terminate() }, timeoutMs)
      let lastUpdate = 0
      const details = (exitCode: number | null, sig: string | null): ShellDetails => ({
        command: params.command, description: params.description, cwd, exitCode, signal: sig, timedOut, durationMs: Date.now() - started,
      })
      const update = (): void => {
        const now = Date.now()
        if (onUpdate === undefined || now - lastUpdate < 200) return
        lastUpdate = now
        onUpdate({ content: [{ type: 'text', text: renderShellResult(stdout.text(), stderr.text(), { exitCode: 0, signal: null, timedOut: false, timeoutMs }) }], details: details(null, null) })
      }
      const pump = async (stream: NodeJS.ReadableStream, collector: StreamCollector): Promise<void> => {
        for await (const chunk of stream) { await collector.push(chunk as Buffer); update() }
        await collector.close()
      }
      try {
        const [outcome] = await Promise.all([handle.done, pump(handle.stdout, stdout), pump(handle.stderr, stderr)])
        if (signal?.aborted) throw new Error('tool call aborted')
        const text = renderShellResult(stdout.text(), stderr.text(), { ...outcome, timedOut, timeoutMs })
        return textResult(await limitOutput(context, shell, text), details(outcome.exitCode, outcome.signal))
      } finally {
        clearTimeout(timer)
        handle.terminate()
        await handle.waitForExit()
      }
    },
  }
}
