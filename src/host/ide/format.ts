/** Explicit formatting of human editor buffers through maintained, locally shipped tools. */
import { join } from 'node:path'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { SubprocessHandle, SubprocessRuntime } from '@deepseek-ai/dsh-subprocess'
import { assertNever } from '@deepseek-ai/dsh-util-values'
import type { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import { z } from 'zod'
import { IdeOperationError, ideRelativePath } from './ide-files-core.ts'
import type { IdeToolPaths } from './ide-tools.ts'

/** Editor language identifiers supported by the first IDE release. */
export const ideLanguageSchema = z.enum(['python', 'javascript', 'typescript', 'c', 'cpp'])
/** Language identifier shared by formatter and language-server selection. */
export type IdeLanguage = z.infer<typeof ideLanguageSchema>
const requestSchema = z.object({ op: z.literal('format'),
  workspaceId: z.string().min(1).max(512).transform(value => brandString<WorkspaceId>(value)),
  path: z.string().min(1).max(32768), text: z.string(), language: ideLanguageSchema,
}).strict()

/** Formatter process bounds supplied by the owning Host configuration. */
export interface IdeFormatLimits {
  readonly maxTextBytes: number
  readonly maxStderrBytes: number
  readonly timeoutMs: number
  readonly killGraceMs: number
}

/** Format an unsaved buffer without writing its file.
 * @param body - decoded, untrusted HTTP request.
 * @param options - explicit execution provider, workspace resolution, helper paths and limits.
 * @returns formatted text; a timeout or tool failure preserves the original editor buffer.
 */
export async function formatIdeDocument(body: unknown, options: {
  readonly subprocess: SubprocessRuntime
  readonly resolveWorkspace: (id: WorkspaceId) => Promise<{ readonly root: string }>
  readonly tools: IdeToolPaths
  readonly limits: IdeFormatLimits
  readonly signal?: AbortSignal
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
  let child: SubprocessHandle | undefined
  try {
    child = options.subprocess.spawn({ argv, cwd: root,
      stdio: { stdin: { data: request.text }, stdout: { maxBytes: options.limits.maxTextBytes },
        stderr: { maxBytes: options.limits.maxStderrBytes } },
      graceMs: options.limits.killGraceMs, signal: abort.signal, env: { PYTHONDONTWRITEBYTECODE: '1' },
    })
    const result = await child.done
    abort.signal.throwIfAborted()
    const text = child.collected.stdout?.readFrom(0)
    if (result.exitCode !== 0 || result.signal !== null || !text || text.lossy) throw new IdeOperationError('io-error', child.collected.stderr?.readFrom(0).text || 'Formatting did not produce a complete result')
    return { text: request.text.includes('\r\n') && !/(?<!\r)\n/u.test(request.text) ? text.text.replace(/\r?\n/g, '\r\n') : text.text }
  } finally {
    clearTimeout(deadline)
    options.signal?.removeEventListener('abort', cancel)
    child?.terminate()
    await child?.waitForExit()
  }
}
