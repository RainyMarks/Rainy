/** Interactive answers: the `show_widget` tool's arguments and the messages between a chat widget and its sandboxed frame.
 *  The idea and the streaming order (styles, then markup, then script) follow CopilotKit's Open Intelligent UI (MIT). */

/** Name of the agent tool that shows an interactive answer. */
export const WIDGET_TOOL = 'show_widget'
/** Address of the frame document; it and its libraries are served without the session cookie, which a sandboxed frame never sends. */
export const WIDGET_FRAME_PATH = '/rainy/widget/frame'
/** Tag on every message, so stray messages from other frames and extensions are ignored. */
export const WIDGET_MESSAGE = 'rainy-widget'

/** Upper bound of one widget's styles, markup and script together, in characters. */
export const WIDGET_MAX_CHARS = 200_000
export const WIDGET_MIN_HEIGHT = 40
export const WIDGET_MAX_HEIGHT = 2400
export const WIDGET_DEFAULT_HEIGHT = 240
/** A prompt a widget sends on the user's behalf is a short message, not a document. */
export const WIDGET_PROMPT_MAX_CHARS = 4000

/** The tool's arguments as the model writes them; every field may still be missing while they stream. */
export interface WidgetArgs {
  title: string
  height: number
  css: string
  html: string
  script: string
}

/**
 * Read possibly partial tool arguments.
 * @param value The call's `arguments`.
 * @returns Strings for every field, empty when absent, and the initial height clamped to the allowed range.
 */
export function widgetArgs(value: unknown): WidgetArgs {
  const record = value !== null && typeof value === 'object' ? value as Record<string, unknown> : {}
  const text = (field: string): string => typeof record[field] === 'string' ? record[field] : ''
  const height = typeof record.height === 'number' && Number.isFinite(record.height) ? record.height : WIDGET_DEFAULT_HEIGHT
  return { title: text('title').trim(), height: clampHeight(height), css: text('css'), html: text('html'), script: text('script') }
}

/** @param height Reported or requested height in CSS pixels. @returns The height the frame gets. */
export function clampHeight(height: number): number {
  return Math.min(WIDGET_MAX_HEIGHT, Math.max(WIDGET_MIN_HEIGHT, Math.ceil(height)))
}

/**
 * Markup that is still being written, made safe to show: the unfinished tag and entity at the end are cut, and styles,
 * scripts and the head are dropped (styles arrive in their own field; scripts run once the answer is complete).
 * Adapted from Open Intelligent UI's `processPartialHtml`.
 * @param html Markup so far.
 * @returns Body markup to show while the rest streams.
 */
export function partialHtml(html: string): string {
  let result = html.replace(/<[^>]*$/u, '')
  result = result.replace(/<(style|script|head)\b[^>]*>[\s\S]*?<\/\1\s*>/giu, '')
  result = result.replace(/<(style|script|head)\b[^>]*>[\s\S]*$/iu, '')
  result = result.replace(/&[a-zA-Z0-9#]*$/u, '')
  const body = /<body[^>]*>([\s\S]*)/iu.exec(result)
  if (body !== null) result = body[1]!.replace(/<\/body>[\s\S]*/iu, '')
  return result
}

/** Theme values the frame turns into CSS variables, by variable name without the leading dashes. */
export type WidgetTheme = { dark: boolean; vars: Record<string, string> }

/** Page → frame. */
export type WidgetHostMessage =
  | { tag: typeof WIDGET_MESSAGE; type: 'theme'; theme: WidgetTheme }
  /** Partial content while the model writes it; markup is already passed through {@link partialHtml}. */
  | { tag: typeof WIDGET_MESSAGE; type: 'preview'; css: string; html: string }
  /** The finished answer; shown once, after which its scripts run. */
  | { tag: typeof WIDGET_MESSAGE; type: 'final'; css: string; html: string; script: string }
  /** Answer to a bridge call. */
  | { tag: typeof WIDGET_MESSAGE; type: 'reply'; id: number; ok: boolean; error?: string | undefined }

/** Frame → page. */
export type WidgetFrameMessage =
  | { tag: typeof WIDGET_MESSAGE; type: 'ready' }
  | { tag: typeof WIDGET_MESSAGE; type: 'resize'; height: number }
  | { tag: typeof WIDGET_MESSAGE; type: 'error'; message: string }
  /** `rainy.sendPrompt(text)`: send a chat message for the user, after a click in the widget. */
  | { tag: typeof WIDGET_MESSAGE; type: 'prompt'; id: number; text: string }
  /** `rainy.openLink(url)`: open an http(s) page in the browser. */
  | { tag: typeof WIDGET_MESSAGE; type: 'link'; id: number; url: string }

/**
 * Validate a message from a frame.
 * @param data `MessageEvent.data`.
 * @returns The message, or `undefined` when it is not one.
 */
export function frameMessage(data: unknown): WidgetFrameMessage | undefined {
  if (data === null || typeof data !== 'object') return undefined
  const message = data as Record<string, unknown>
  if (message.tag !== WIDGET_MESSAGE) return undefined
  switch (message.type) {
    case 'ready': return { tag: WIDGET_MESSAGE, type: 'ready' }
    case 'resize': return typeof message.height === 'number' && Number.isFinite(message.height) ? { tag: WIDGET_MESSAGE, type: 'resize', height: message.height } : undefined
    case 'error': return { tag: WIDGET_MESSAGE, type: 'error', message: String(message.message ?? '').slice(0, 500) }
    case 'prompt':
      return typeof message.id === 'number' && typeof message.text === 'string' ? { tag: WIDGET_MESSAGE, type: 'prompt', id: message.id, text: message.text } : undefined
    case 'link':
      return typeof message.id === 'number' && typeof message.url === 'string' ? { tag: WIDGET_MESSAGE, type: 'link', id: message.id, url: message.url } : undefined
    default: return undefined
  }
}

/**
 * A standalone page with the widget, for "Copy as HTML": the same styles, markup and script without the bridge.
 * @param args The finished arguments.
 * @returns HTML document text.
 */
export function standaloneHtml(args: WidgetArgs): string {
  const title = args.title.replace(/[<&]/gu, character => character === '<' ? '&lt;' : '&amp;')
  return `<!doctype html>\n<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>${title}</title>\n`
    + `<style>\n${args.css}\n</style></head>\n<body>\n${args.html}\n${args.script.trim() === '' ? '' : `<script>\n${args.script.replace(/<\/script/giu, '<\\/script')}\n</script>\n`}</body></html>\n`
}
