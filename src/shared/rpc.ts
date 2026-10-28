/**
 * Host ↔ page protocol carried by the `/rpc` WebSocket, as far as the viewer pages use it; `native/crates/protocol` is
 * the whole protocol, which the native interface speaks.
 *
 * Frames are JSON objects:
 * - request  `{ id, method, params }` from the page
 * - response `{ id, result }` or `{ id, error: RpcErrorBody }` from the Host
 * - event    `{ event, data }` from the Host, broadcast to every connected page
 */
import type { StreamPatch } from './stream-patch.ts'
import type { Accent, ColorScheme } from './appearance.ts'
import type { IdeFilesRequest, IdeFilesResults, WorkspaceId } from './ide-files-protocol.ts'
import type { MapExplanation, ProjectMap } from './map-protocol.ts'

export type { WorkspaceId }

/** Chat identifier: a UUID string. */
export type SessionId = string

// ───────────────────────────── Messages ─────────────────────────────
// The shapes of `rainy-protocol`'s `chat` module (pi-ai's in 3.x), as far as pages read them.

export interface TextContent { type: 'text'; text: string; textSignature?: string | undefined }
export interface ThinkingContent { type: 'thinking'; thinking: string; thinkingSignature?: string | undefined; redacted?: boolean | undefined }
export interface ToolCall { type: 'toolCall'; id: string; name: string; arguments: Record<string, unknown> }

/** One model reply. */
export interface AssistantMessage {
  role: 'assistant'
  content: (TextContent | ThinkingContent | ToolCall)[]
  api: string
  provider: string
  model: string
  usage: { input: number; output: number; cacheRead: number; cacheWrite: number }
  stopReason: 'stop' | 'length' | 'toolUse' | 'error' | 'aborted'
  errorMessage?: string | undefined
  timestamp: number
}

// ───────────────────────────── Preferences ─────────────────────────────

/** Interface preferences kept by the Host, so every page and the native interface share them. */
export interface UiPreferences {
  locale: 'zh' | 'en'
  theme: 'system' | 'light' | 'dark'
  uiFontSize: number
  codeFontSize: number
  /** What Enter does while the agent is busy. */
  busyEnter: 'queue' | 'steer'
  /** How much of each turn's tool activity is expanded by default. */
  stepDetail: 'compact' | 'standard' | 'detailed'
  /** Whether per-turn token usage is shown. */
  showUsage: boolean
  /** Gray ramp of every surface and label. */
  colorScheme: ColorScheme
  /** Color of links, focus rings and switches; `auto` follows the color scheme. */
  accent: Accent
  /** Interface font families before the default ones, separated by commas; empty for the default. */
  uiFont: string
  /** Code font families before the default ones, separated by commas; empty for the default. */
  codeFont: string
  /** Whether the editor asks the model for ghost-text suggestions while typing. */
  inlineCompletion: boolean
  /** Provider id of the model behind suggestions; empty to use the chat model. */
  completionModel: string
  /** The agent's changes are kept when the next message starts a run, without clicking Keep on each file. */
  autoKeepChanges: boolean
  /** Empty views show falling rain, and a click on them makes a ripple. */
  rainEffects: boolean
  /** Chats get web_search and web_fetch when a DeepSeek model has an API key. */
  webSearch: boolean
  /** Typora's focus mode in the rich Markdown editor: blocks other than the one being written fade. */
  markdownFocus: boolean
  /** Typora's typewriter mode: the line being written stays in the middle of the view. */
  markdownTypewriter: boolean
  /** Where images pasted or dropped into a Markdown document are saved: `assets` beside it, `<name>.assets`, or its own folder. */
  markdownImages: 'assets' | 'named' | 'same'
}

// ───────────────────────────── Chats ─────────────────────────────

/** A chat of the history list, as far as the project map reads it. */
export interface SessionSummary {
  id: SessionId
  /** The chat's working directory, which relative tool paths start from. */
  cwd: string
}

export type SessionRunStatus = 'idle' | 'running' | 'compacting' | 'error'

/** A file the agent changed in a chat whose changes the user has neither kept nor undone. */
export interface ChatChange {
  /** Absolute path on the Host. */
  path: string
  /** Path as the chat shows it: relative to its working directory when inside it. */
  display: string
  /** Write and edit calls behind the change, oldest first. */
  toolCallIds: string[]
  /** The agent created the file. */
  created: boolean
  /** The file no longer exists. */
  deleted: boolean
  /** Lines added and removed since before the first change; 0 when the change cannot be compared. */
  added: number
  removed: number
  /** The text before the changes is kept, so they can be reviewed side by side. */
  comparable: boolean
  /** Every call kept the content it replaced, so the change can be undone. */
  undoable: boolean
}

// ───────────────────────────── Method table ─────────────────────────────

/** The file operations of the `ide` method that pages make. */
export type IdeRequest = IdeFilesRequest
/** Result type for one IDE operation. */
export type IdeResult<T extends IdeRequest> = IdeFilesResults[T['op']]

/** The methods pages call: `[params, result]`. */
export interface HostMethods {
  'prefs.get': [void, UiPreferences]
  'sessions.list': [{ workspaceId?: WorkspaceId | null | undefined; archived?: boolean | undefined }, SessionSummary[]]
  /** Files the chat's agent changed that are not yet kept or undone. */
  'chat.changes': [{ sessionId: SessionId }, ChatChange[]]
  /** The project map; a recent one is reused unless `refresh` is set. */
  'map.get': [{ workspaceId: WorkspaceId; refresh?: boolean | undefined }, ProjectMap]
  /** Ask the chat model to describe the project's parts; the answer is kept with the project. */
  'map.explain': [{ workspaceId: WorkspaceId }, MapExplanation]
  'ide': [IdeRequest, unknown]
}

export type HostMethod = keyof HostMethods
export type MethodParams<M extends HostMethod> = HostMethods[M][0]
export type MethodResult<M extends HostMethod> = HostMethods[M][1]

/** The events pages follow, as far as they read them. */
export interface HostEvents {
  /**
   * Streaming progress of the current request: `message` is the whole partial assistant message (`null` once it ended),
   * `patch` what was added since the previous event (`shared/stream-patch.ts`). The whole message comes first and again
   * every few seconds, so a page that missed an update or loaded the chat mid-reply catches up.
   */
  'session.stream': { sessionId: SessionId; message: AssistantMessage | null } | { sessionId: SessionId; patch: StreamPatch }
  /** The chat's list of changed files may have changed: a write or edit finished, or changes were kept or undone. */
  'session.changes': { sessionId: SessionId }
  /** Partial output of a running tool. */
  'session.tool': { sessionId: SessionId; toolCallId: string; toolName: string; phase: 'start' | 'update' | 'end'; args?: unknown; partial?: string | undefined }
  /** Run status, queue or model changed. */
  'session.state': { sessionId: SessionId; status: SessionRunStatus }
  /** A chat was created or its summary changed. */
  'sessions.changed': SessionSummary
  'prefs.changed': UiPreferences
}
export type HostEvent = keyof HostEvents

/** Error body of a failed request; `code` is stable, such as an IDE error code (`already-exists`). */
export interface RpcErrorBody {
  code: string
  message: string
}

export type RpcResponseFrame = { id: number; result?: unknown; error?: RpcErrorBody }
export type RpcEventFrame = { event: HostEvent; data: unknown }
