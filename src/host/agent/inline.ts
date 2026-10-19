/** Editor AI: ghost-text suggestions at the cursor and rewriting a selection for an instruction (Ctrl+K). */
import { createInitialSystemMessage, normalizeContext, type AssistantMessage } from '@earendil-works/pi-ai'
import { localized } from '../../shared/locale.ts'
import type { InlineCompletionRequest, InlineCompletionResult, InlineEditRequest, UiPreferences } from '../../shared/rpc.ts'
import type { Settings } from '../settings.ts'
import type { Models, ResolvedModel } from './models.ts'

/** Characters of the file sent before and after the cursor for a suggestion. */
const SUGGEST_BEFORE = 3000
const SUGGEST_AFTER = 1000
/** A suggestion is short; a longer reply is cut. */
const SUGGEST_MAX_TOKENS = 192
const SUGGEST_MAX_LINES = 16
const SUGGEST_TIMEOUT_MS = 15_000
/** Characters of the file sent around a selection to rewrite. */
const EDIT_BEFORE = 6000
const EDIT_AFTER = 3000
const EDIT_TIMEOUT_MS = 180_000
/** Longest gap between progress events while a rewrite streams. */
const PROGRESS_INTERVAL_MS = 80

const CURSOR = '<|cursor|>'
const SELECTION_START = '<|selection_start|>'
const SELECTION_END = '<|selection_end|>'

const SUGGEST_SYSTEM = [
  'You are a code completion engine inside an editor. The file is shown with <|cursor|> marking the cursor.',
  'Reply with exactly the text to insert at the cursor: no explanation, no Markdown, no code fences, and nothing that is already before or after the cursor.',
  'Complete the current line, or the current statement or small block when the context makes the next lines clear.',
  'If nothing useful should be inserted, reply with an empty message.',
].join(' ')

const EDIT_SYSTEM = [
  'You rewrite code in an editor. The file is shown with the selected region between <|selection_start|> and <|selection_end|>, followed by an instruction.',
  'Reply with only the new text for the selected region: no explanation, no Markdown, no code fences, and nothing outside the region.',
  'Keep the indentation and style of the surrounding code. When the region is empty, reply with the code to insert at that point.',
].join(' ')

/**
 * The request one suggestion sends: the fixed system prompt and the file around the cursor.
 * @param prompt The file with the cursor marked.
 * @returns A normalized context for the stream function.
 */
function suggestionContext(prompt: string): ReturnType<typeof normalizeContext> {
  const system = createInitialSystemMessage(SUGGEST_SYSTEM, [])
  return normalizeContext({ messages: [...(system === undefined ? [] : [system]), { role: 'user', content: prompt, timestamp: Date.now() }] })
}

/** @returns The text blocks of a reply, joined. */
function textOf(message: AssistantMessage): string {
  return message.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('')
}

/** @returns The body of the first fenced code block when the reply has one, otherwise the reply. */
function unfence(text: string): string {
  const fence = /```[^\n`]*\n([\s\S]*?)(?:\n```|$)/u.exec(text)
  return fence === null ? text : fence[1]!
}

/**
 * Turn a model reply into text to insert at the cursor.
 * @param raw Reply.
 * @param request Text around the cursor.
 * @returns The insertion; empty when nothing usable remains.
 */
export function cleanSuggestion(raw: string, request: Pick<InlineCompletionRequest, 'prefix' | 'suffix'>): string {
  let text = unfence(raw).replaceAll(CURSOR, '')
  const linePrefix = request.prefix.slice(request.prefix.lastIndexOf('\n') + 1)
  // Models sometimes repeat the start of the line before continuing it.
  if (linePrefix.trim() !== '' && text.startsWith(linePrefix)) text = text.slice(linePrefix.length)
  const lineSuffix = request.suffix.split('\n', 1)[0]!
  if (lineSuffix.trim() !== '') {
    // In the middle of a line only the rest of that line fits, without what already follows the cursor.
    text = text.split('\n', 1)[0]!
    const tail = lineSuffix.trim()
    if (text.trimEnd().endsWith(tail)) text = text.trimEnd().slice(0, -tail.length)
  }
  const lines = text.split('\n').slice(0, SUGGEST_MAX_LINES)
  text = lines.join('\n').trimEnd()
  return text.trim() === '' ? '' : text
}

/**
 * Turn a model reply into the replacement of a selection.
 * @param raw Reply so far.
 * @param selection The selected text it replaces.
 * @returns The replacement.
 */
export function cleanEdit(raw: string, selection: string): string {
  let text = unfence(raw).replaceAll(SELECTION_START, '').replaceAll(SELECTION_END, '')
  if (!selection.startsWith('\n')) text = text.replace(/^(?:[ \t]*\r?\n)+/u, '')
  if (!selection.endsWith('\n')) text = text.replace(/(?:\r?\n[ \t]*)+$/u, '')
  return text
}

/** @returns The file header of a prompt. */
function header(request: { path: string; language?: string | undefined }): string {
  return `File: ${request.path}${request.language === undefined || request.language === '' ? '' : ` (${request.language})`}`
}

/** Suggestions and rewrites for the editor; the chat model, or another one for suggestions, answers them. */
export class InlineAssist {
  private suggestion: AbortController | undefined
  private readonly edits = new Map<string, AbortController>()

  /**
   * @param models Model settings and the request function.
   * @param settings Preferences: whether suggestions are on and which model gives them.
   * @param progress Receives the replacement a rewrite has produced so far.
   */
  constructor(
    private readonly models: Models,
    private readonly settings: Pick<Settings, 'prefs' | 'get'>,
    private readonly progress: (requestId: string, text: string) => void,
  ) {}

  /**
   * A ghost-text suggestion. A newer request ends the previous one, and failures come back as `error` so typing goes on.
   * @param request Text around the cursor.
   * @returns The insertion.
   */
  async complete(request: InlineCompletionRequest): Promise<InlineCompletionResult> {
    this.suggestion?.abort()
    const prefs: UiPreferences = this.settings.prefs()
    if (!prefs.inlineCompletion) return { text: '' }
    const resolved = this.suggestionModel(prefs.completionModel)
    if (resolved === undefined) return { text: '', error: noModel() }
    const controller = new AbortController()
    this.suggestion = controller
    const prompt = `${header(request)}\n\n${request.prefix.slice(-SUGGEST_BEFORE)}${CURSOR}${request.suffix.slice(0, SUGGEST_AFTER)}`
    const requestId = request.requestId
    try {
      // The suggestion streams, so its first line appears while the rest is still being written.
      const stream = this.models.streamFn(resolved.model, suggestionContext(prompt) as never, {
        signal: AbortSignal.any([controller.signal, AbortSignal.timeout(SUGGEST_TIMEOUT_MS)]), cacheRetention: 'none',
      })
      let sent = 0
      for await (const event of stream) {
        if (requestId === undefined || !('partial' in event) || Date.now() - sent < PROGRESS_INTERVAL_MS) continue
        sent = Date.now()
        const partial = cleanSuggestion(textOf(event.partial), request)
        if (partial !== '') this.progress(requestId, partial)
      }
      const reply = await stream.result()
      if (reply.stopReason === 'aborted' || controller.signal.aborted) return { text: '' }
      if (reply.stopReason === 'error') return { text: '', error: reply.errorMessage ?? localized({ zh: '补全请求失败。', en: 'The suggestion request failed.' }) }
      return { text: cleanSuggestion(textOf(reply), request) }
    } finally {
      if (this.suggestion === controller) this.suggestion = undefined
    }
  }

  /**
   * Rewrite a selection with the chat model, reporting the text as it streams.
   * @param request Selection, its surroundings and the instruction.
   * @returns The replacement.
   * @throws Error when no model is set up, the request fails or it was cancelled.
   */
  async edit(request: InlineEditRequest): Promise<{ text: string }> {
    const resolved = this.models.resolve()
    if (resolved === undefined) throw new Error(noModel())
    this.edits.get(request.requestId)?.abort()
    const controller = new AbortController()
    this.edits.set(request.requestId, controller)
    const file = `${request.before.slice(-EDIT_BEFORE)}${SELECTION_START}${request.selection}${SELECTION_END}${request.after.slice(0, EDIT_AFTER)}`
    const prompt = `${header(request)}\n\n${file}\n\nInstruction: ${request.instruction}`
    const system = createInitialSystemMessage(EDIT_SYSTEM, [])
    const context = normalizeContext({ messages: [...(system === undefined ? [] : [system]), { role: 'user', content: prompt, timestamp: Date.now() }] })
    try {
      const stream = this.models.streamFn(resolved.model, context as never, {
        signal: AbortSignal.any([controller.signal, AbortSignal.timeout(EDIT_TIMEOUT_MS)]), cacheRetention: 'none',
      })
      let sent = 0
      for await (const event of stream) {
        if (!('partial' in event) || Date.now() - sent < PROGRESS_INTERVAL_MS) continue
        sent = Date.now()
        this.progress(request.requestId, cleanEdit(textOf(event.partial), request.selection))
      }
      const reply = await stream.result()
      if (reply.stopReason === 'aborted' || controller.signal.aborted) throw new Error(localized({ zh: '已停止生成。', en: 'Generation was stopped.' }))
      if (reply.stopReason === 'error') throw new Error(reply.errorMessage ?? localized({ zh: '修改请求失败。', en: 'The edit request failed.' }))
      return { text: cleanEdit(textOf(reply), request.selection) }
    } finally {
      if (this.edits.get(request.requestId) === controller) this.edits.delete(request.requestId)
    }
  }

  /** @param requestId Rewrite to stop. */
  cancel(requestId: string): void {
    this.edits.get(requestId)?.abort()
  }

  /** Stop every request, for shutdown. */
  close(): void {
    this.suggestion?.abort()
    for (const controller of this.edits.values()) controller.abort()
  }

  private suggestionModel(provider: string): ResolvedModel | undefined {
    const setup = provider === '' ? undefined : this.settings.get().models.find(model => model.provider === provider)
    return this.models.resolve(setup === undefined ? undefined : { provider: setup.provider, model: setup.model })
  }
}

function noModel(): string {
  return localized({ zh: '还没有可用的模型。请先在“设置 → 模型”中添加一个模型。', en: 'No model is set up yet. Add one in Settings → Models.' })
}
