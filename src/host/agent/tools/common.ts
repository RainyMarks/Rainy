/** Shared pieces of the model-facing tools: path resolution, read-before-write tracking and output limits. */
import { randomBytes } from 'node:crypto'
import { mkdir, stat, writeFile } from 'node:fs/promises'
import { isAbsolute, join, relative, resolve, sep } from 'node:path'
import type { AgentToolResult } from '@earendil-works/pi-agent-core'
import { estimateText } from '../../../shared/budget.ts'
import type { ResolvedWorkspaceEnvironment } from '../../runtime/environments.ts'

/** What every tool of one chat needs. */
export interface ToolContext {
  /** Chat working directory (the project's primary directory). */
  cwd: string
  /** Chat id, exported to commands as `RAINY_SESSION_ID`. */
  sessionId: string
  /** Process environment overlay for interpreters selected in Settings → Runtime. */
  runtimeEnvironment(cwd: string): ResolvedWorkspaceEnvironment
  /** Directory for full tool outputs that were cut for the model. */
  spillDir: string
  /** Token budget of one tool result for the current model. */
  toolTokens(): number
  /** File versions this chat has read or written. */
  observations: Observations
  /** PowerShell executable on Windows. */
  pwshPath?: string | undefined
  /** Called after a tool reads or changes a file, with its absolute path. */
  onFileTouched?: ((path: string) => void) | undefined
}

/** A file's identity at one observation. */
export interface FileVersion { size: number; mtimeMs: number; ino: number }

/** Files a chat has seen; write and edit refuse to change a file the chat has not read in its current state. */
export class Observations {
  private readonly versions = new Map<string, FileVersion | 'absent'>()

  /**
   * Record what the chat last saw.
   * @param path Absolute path.
   * @param version The version, or `'absent'` when the file did not exist.
   */
  record(path: string, version: FileVersion | 'absent'): void {
    this.versions.set(key(path), version)
  }

  /**
   * Check that a change is based on the file's current state.
   * @param path Absolute path.
   * @param current The file's version now, or `undefined` when it does not exist.
   * @param displayPath Path shown in errors.
   * @param verb `write` or `modify`, used in the error text.
   */
  assertCurrent(path: string, current: FileVersion | undefined, displayPath: string, verb: 'write' | 'modify'): void {
    if (current === undefined) return
    const seen = this.versions.get(key(path))
    if (seen === undefined || seen === 'absent') {
      throw new Error(`cannot ${verb === 'write' ? 'modify' : verb} "${displayPath}": file has not been read — read the file, then retry`)
    }
    if (seen.size !== current.size || seen.mtimeMs !== current.mtimeMs || seen.ino !== current.ino) {
      throw new Error(`cannot ${verb} "${displayPath}": file changed since it was read — re-read the file, then retry`)
    }
  }
}

function key(path: string): string {
  return process.platform === 'win32' ? path.toLowerCase() : path
}

/**
 * Current version of a file.
 * @param path Absolute path.
 * @returns Its version, or `undefined` when it does not exist.
 */
export async function versionOf(path: string): Promise<FileVersion | undefined> {
  try {
    const info = await stat(path, { bigint: false })
    return { size: info.size, mtimeMs: info.mtimeMs, ino: info.ino }
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined
    throw error
  }
}

/**
 * Resolve a tool path against the chat's working directory.
 * @param context Tool context.
 * @param requested Path from the model; relative paths resolve against the cwd.
 * @returns The absolute path and the spelling shown to the model.
 */
export function resolvePath(context: ToolContext, requested: string): { absolute: string; display: string } {
  if (requested.trim() === '' || requested.includes('\0')) throw new Error('invalid file_path: expected a non-empty path')
  const absolute = resolve(context.cwd, requested)
  const inside = relative(context.cwd, absolute)
  const display = inside !== '' && !inside.startsWith(`..${sep}`) && inside !== '..' && !isAbsolute(inside) ? inside.split(sep).join('/') : absolute
  return { absolute, display }
}

/**
 * A text-only tool result.
 * @param text Model-facing text.
 * @param details Structured data for the UI card.
 * @returns The result object.
 */
export function textResult<T>(text: string, details: T): AgentToolResult<T> {
  return { content: [{ type: 'text', text }], details }
}

/**
 * Keep a large tool output within the model's tool budget: the full text is saved to a file and the model receives
 * the head and tail with a pointer to that file.
 * @param context Tool context.
 * @param toolName Tool name, used in the file name.
 * @param text Full output.
 * @returns The text to send to the model.
 */
export async function limitOutput(context: ToolContext, toolName: string, text: string): Promise<string> {
  const cap = context.toolTokens()
  if (estimateText(text) <= cap) return text
  await mkdir(context.spillDir, { recursive: true, mode: 0o700 })
  const path = join(context.spillDir, `${randomBytes(6).toString('hex')}-${toolName}.txt`)
  await writeFile(path, text, { mode: 0o600 })
  const note = `\n[Output limited; full result: ${path}]\nUse read with offset/limit, or grep this path to search within it.\n`
  let window = cap * 3
  for (;;) {
    const head = text.slice(0, Math.floor(window * 0.75))
    const tail = text.slice(text.length - Math.floor(window * 0.25))
    const candidate = `${head}${note}${tail}`
    if (estimateText(candidate) <= cap || window < 64) return candidate
    window = Math.floor(window * 0.8)
  }
}
