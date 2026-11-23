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
import type { IdeFilesRequest, IdeFilesResults, OfficePdfRegistration, WorkspaceId } from './ide-files-protocol.ts'
import type { FootprintTurn, MapExplanation, ProjectMap } from './map-protocol.ts'

export type { OfficePdfRegistration, WorkspaceId }

/** Chat identifier: a UUID string. */
export type SessionId = string

// ───────────────────────────── Messages ─────────────────────────────
// The shapes of `rainy-protocol`'s `chat` module (pi-ai's in 3.x), as far as pages read them.

export interface TextContent { type: 'text'; text: string; textSignature?: string | undefined }
export interface ThinkingContent { type: 'thinking'; thinking: string; thinkingSignature?: string | undefined; redacted?: boolean | undefined }
export interface ToolCall { type: 'toolCall'; id: string; name: string; arguments: Record<string, unknown> }

/** Token counts of one model request (`rainy-protocol::chat::Usage`). */
export interface Usage {
  input: number
  output: number
  cacheRead: number
  cacheWrite: number
  reasoning?: number | undefined
}

/** One model reply. */
export interface AssistantMessage {
  role: 'assistant'
  content: (TextContent | ThinkingContent | ToolCall)[]
  api: string
  provider: string
  model: string
  usage: Usage
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
  /** How a full chat context is freed once old tool outputs are cleared: an index read back with the recall tool, or a summary the model writes (4.6's `auto` reads back). */
  contextStrategy: 'recall' | 'summary' | 'auto'
  /** Windows notifications when a chat finishes or waits for an approval while Rainy is in the background. */
  notify: boolean
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

/** A tool call waiting for the user (`rainy-protocol::chat::PendingApproval`). */
export interface PendingApproval {
  toolCallId: string
  toolName: string
  reason: 'command' | 'outside-write' | 'outside-read' | 'outside-workdir' | 'mcp' | 'web'
  rule?: string | null | undefined
  path?: string | null | undefined
  workdir?: string | null | undefined
}

/** Context-window accounting for the next request (`rainy-protocol::chat::ContextUsage`). */
export interface ContextUsage {
  tokens: number
  contextWindow: number
  inputLimit: number
  outputTokens: number
  compactAt: number
  kind: string
}

/** Token breakdown by context category (`rainy-protocol::chat::ContextBreakdownSegments`). */
export interface ContextBreakdownSegments {
  system: number
  instructions: number
  mcpTools: number
  coreTools: number
  history: number
  recall: number
}

/** Detailed context-window breakdown for one chat (`session.contextBreakdown`). */
export interface ContextBreakdown {
  sessionId: SessionId
  contextWindow: number
  inputLimit: number
  compactAt: number
  outputTokens: number
  total: number
  calibratedTotal: number
  calibrationFactor: number
  kind: string
  segments: ContextBreakdownSegments
}

// ───────────────────────────── IDE & Execution ─────────────────────────────
// Shared shapes mirrored with `native/crates/protocol/src/ide.rs`.

export interface IdeOk {
  ok: boolean
}

export interface ProcessExit {
  exitCode?: number | null | undefined
  signal?: string | null | undefined
  stopped: boolean
}

export interface TerminalInfo {
  id: string
  workspaceId?: WorkspaceId | null | undefined
  cwd: string
  phase: 'starting' | 'running' | 'stopping' | 'exited' | 'failed'
  exit?: ProcessExit | null | undefined
  error?: string | null | undefined
}

export interface RunSpec {
  terminal: boolean
  name?: string | null | undefined
  language?: string | null | undefined
  argv: string[]
  cwd?: string | null | undefined
}

export interface RunInfo {
  id: string
  workspaceId?: WorkspaceId | null | undefined
  name: string
  phase: 'starting' | 'building' | 'running' | 'stopping' | 'exited' | 'failed'
  spec: RunSpec
  exit?: ProcessExit | null | undefined
  error?: string | null | undefined
}

export interface DebugCapabilities {
  evaluate: boolean
  pause: boolean
  stepIn: boolean
  stepOut: boolean
  next: boolean
}

export interface DebugBreakpoint {
  path: string
  requestedLine: number
  verified: boolean
  id?: number | null | undefined
  line?: number | null | undefined
  message?: string | null | undefined
}

export interface DebugInfo {
  id: string
  workspaceId?: WorkspaceId | null | undefined
  name: string
  language?: string | null | undefined
  rootId?: string | null | undefined
  phase: 'starting' | 'building' | 'initializing' | 'running' | 'paused' | 'stopping' | 'terminated' | 'failed'
  threadId?: number | null | undefined
  reason?: string | null | undefined
  capabilities?: DebugCapabilities | null | undefined
  breakpoints: DebugBreakpoint[]
  exit?: ProcessExit | null | undefined
  error?: string | null | undefined
}

export interface ExecutionStatus {
  runs: RunInfo[]
  terminals: TerminalInfo[]
  debugSessions: DebugInfo[]
}

export interface ExecutionEvent {
  sequence: number
  workspaceId?: WorkspaceId | null | undefined
  kind: 'output' | 'run' | 'terminal' | 'debug'
  operationId?: string | null | undefined
  stream?: string | null | undefined
  text?: string | null | undefined
  run?: unknown
  terminal?: unknown
  debug?: unknown
}

/** Response of `execution.poll`: ordered events after the requested cursor plus whether older output was dropped. */
export interface ExecutionPoll {
  events: ExecutionEvent[]
  cursor: number
  truncated: boolean
  status: ExecutionStatus
}

// ───────────────────────────── Method table ─────────────────────────────

/** The file operations of the `ide` method that pages make. */
export type IdeRequest = IdeFilesRequest
/** Result type for one IDE operation. */
export type IdeResult<T extends IdeRequest> = IdeFilesResults[T['op']]
/** Union of all strongly typed results returned by the `ide` RPC method. */
export type IdeResponse = IdeFilesResults[IdeRequest['op']]

/** The methods pages call: `[params, result]`. */
export interface HostMethods {
  'prefs.get': [void, UiPreferences]
  'sessions.list': [{ workspaceId?: WorkspaceId | null | undefined; archived?: boolean | undefined }, SessionSummary[]]
  /** Files the chat's agent changed that are not yet kept or undone. */
  'chat.changes': [{ sessionId: SessionId }, ChatChange[]]
  /** Detailed token usage breakdown for a chat's next request. */
  'session.contextBreakdown': [{ sessionId: SessionId }, ContextBreakdown]
  /** The project map; a recent one is reused unless `refresh` is set. */
  'map.get': [{ workspaceId: WorkspaceId; refresh?: boolean | undefined }, ProjectMap]
  /** Ask the chat model to describe the project's parts; the answer is kept with the project. */
  'map.explain': [{ workspaceId: WorkspaceId }, MapExplanation]
  /** What the chat's agent did in each of its recent turns, newest last. */
  'map.footprint': [{ sessionId: SessionId }, { turns: FootprintTurn[] }]
  /** Register a converted temporary Office PDF for one-shot streaming consumption. */
  'ide.registerOfficePdf': [{ path: string; deleteAfterRead?: boolean | undefined }, OfficePdfRegistration]
  'ide': [IdeRequest, IdeResponse]
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
  data?: unknown
}

export type RpcResponseFrame = { id: number; result?: unknown; error?: RpcErrorBody }
export type RpcEventFrame = { event: HostEvent; data: unknown }
