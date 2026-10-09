/** Formatting of unsaved editor buffers with locally shipped tools; the file itself is never written. */
import { join } from 'node:path'
import type { Readable } from 'node:stream'
import { z } from 'zod'
import { assertNever, brandString } from '../../shared/brand.ts'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { ProcessHandle } from '../process.ts'
import type { IdeSubprocess } from './execution-process.ts'
import { IdeOperationError, ideRelativePath } from './files-core.ts'
import type { IdeToolPaths } from './tools.ts'

/** Editor languages with formatting and language-server support. */
export const ideLanguageSchema = z.enum(['python', 'javascript', 'typescript', 'c', 'cpp'])
/** Language identifier shared by formatter and language-server selection. */
export type IdeLanguage = z.infer<typeof ideLanguageSchema>
const requestSchema = z.object({ op: z.literal('format'),
  workspaceId: z.string().min(1).max(512).transform(value => brandString<WorkspaceId>(value)),
  path: z.string().min(1).max(32768), text: z.string(), language: ideLanguageSchema,
}).strict()

/** Formatter process bounds. */
export interface IdeFormatLimits {
  readonly maxTextBytes: number
  readonly maxStderrBytes: number
  readonly timeoutMs: number
  readonly killGraceMs: number
}

async function collect(stream: Readable, limit: number): Promise<{ text: string; complete: boolean }> {
  const chunks: Buffer[] = []
  let size = 0
  for await (const chunk of stream) {
    const buffer = chunk as Buffer
    if (size + buffer.length <= limit) chunks.push(buffer)
    else if (size < limit) chunks.push(buffer.subarray(0, limit - size))
    size += buffer.length
  }
  return { text: Buffer.concat(chunks).toString('utf8'), complete: size <= limit }
}

/**
 * Format an unsaved buffer.
 * @param body Untrusted `format` request.
 * @param options Process provider, project resolution, tool paths and limits.
 * @returns Formatted text; a timeout or tool failure leaves the editor buffer to the caller.
 */
export async function formatIdeDocument(body: unknown, options: {
  readonly subprocess: Pick<IdeSubprocess, 'spawn' | 'resolveExecutable'>
  readonly resolveWorkspace: (id: WorkspaceId) => Promise<{ readonly root: string }>
  readonly tools: IdeToolPaths
  readonly limits: IdeFormatLimits
  readonly signal?: AbortSignal | undefined
}): Promise<{ readonly text: string }> {
  const parsed = requestSchema.safeParse(body)
  if (!parsed.success) throw new IdeOperationError('invalid-request', 'Invalid formatter request')
  const request = parsed.data
  ideRelativePath(request.path)
  if (!request.text.isWellFormed() || Buffer.byteLength(request.text) > options.limits.maxTextBytes) throw new IdeOperationError('too-large', 'Editor text exceeds the formatting limit')
  const { root } = await options.resolveWorkspace(request.workspaceId)
  const filename = join(root, request.path)
  let argv: readonly string[]
  switch (request.language) {
    case 'python': argv = [options.tools.ruff, 'format', '--isolated', '--stdin-filename', filename, '-']; break
    case 'javascript': case 'typescript': argv = [options.tools.node, options.tools.prettier, '--no-config', '--no-editorconfig', '--stdin-filepath', filename]; break
    case 'c': case 'cpp': argv = [await options.subprocess.resolveExecutable('clang-format'), '--style=LLVM', `--assume-filename=${filename}`]; break
    default: return assertNever(request.language)
  }
  const abort = new AbortController()
  const cancel = (): void => { abort.abort(new IdeOperationError('aborted', 'Formatting was cancelled')) }
  options.signal?.addEventListener('abort', cancel, { once: true })
  if (options.signal?.aborted) cancel()
  const deadline = setTimeout(() => { abort.abort(new IdeOperationError('aborted', 'Formatting timed out')) }, options.limits.timeoutMs)
  let child: ProcessHandle | undefined
  try {
    child = options.subprocess.spawn({ argv, cwd: root, environment: { PYTHONDONTWRITEBYTECODE: '1' },
      stdin: 'pipe', graceMs: options.limits.killGraceMs, signal: abort.signal })
    child.stdin?.on('error', (_closedInput: unknown) => { /* A formatter that exits early reports through its exit status. */ })
    child.stdin?.end(request.text)
    const [stdout, stderr, result] = await Promise.all([
      collect(child.stdout, options.limits.maxTextBytes), collect(child.stderr, options.limits.maxStderrBytes), child.done,
    ])
    abort.signal.throwIfAborted()
    if (result.exitCode !== 0 || result.signal !== null || !stdout.complete) {
      throw new IdeOperationError('io-error', stderr.text || 'Formatting did not produce a complete result')
    }
    return { text: request.text.includes('\r\n') && !/(?<!\r)\n/u.test(request.text) ? stdout.text.replace(/\r?\n/g, '\r\n') : stdout.text }
  } catch (error) {
    if (abort.signal.aborted && abort.signal.reason instanceof IdeOperationError) throw abort.signal.reason
    throw error
  } finally {
    clearTimeout(deadline)
    options.signal?.removeEventListener('abort', cancel)
    child?.terminate()
    await child?.waitForExit()
  }
}
