/** The editor's model calls: ghost-text suggestions and Ctrl+K rewrites, answered by the Host. */
import { host } from '../rpc.ts'
import { toast } from '../ui/toasts.tsx'
import type { EditorRewriteRequest, EditorSuggestRequest } from './editor-types.ts'

/** Suggestion failures already reported; each message shows once until a suggestion works again. */
let reported: string | undefined
let nextRequest = 0

/**
 * @param request Text around the cursor.
 * @param signal Aborts when the user keeps typing; the Host then drops the request too.
 * @returns The suggestion, or '' for none.
 */
export async function suggest(request: EditorSuggestRequest, signal: AbortSignal): Promise<string> {
  const result = await host.call('inline.complete', request)
  if (signal.aborted) return ''
  if (result.error !== undefined) {
    if (result.error !== reported) toast(result.error, { tone: 'error' })
    reported = result.error
    return ''
  }
  reported = undefined
  return result.text
}

/**
 * @param request Selection, surroundings and instruction.
 * @param progress Receives the replacement as it streams.
 * @param signal Stops the request.
 * @returns The replacement.
 */
export async function rewrite(request: EditorRewriteRequest, progress: (text: string) => void, signal: AbortSignal): Promise<string> {
  const requestId = `edit-${Date.now().toString(36)}-${(nextRequest++).toString(36)}`
  const unsubscribe = host.on('inline.progress', (event) => { if (event.requestId === requestId) progress(event.text) })
  const cancel = (): void => { void host.call('inline.cancel', { requestId }).catch(() => undefined) }
  signal.addEventListener('abort', cancel, { once: true })
  try {
    return (await host.call('inline.edit', { ...request, requestId })).text
  } finally {
    unsubscribe()
    signal.removeEventListener('abort', cancel)
  }
}
