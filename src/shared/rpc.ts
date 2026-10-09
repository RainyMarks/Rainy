/**
 * Host ↔ renderer protocol carried by the `/rpc` WebSocket.
 *
 * Frames are JSON objects:
 * - request  `{ id, method, params }` from the renderer
 * - response `{ id, result }` or `{ id, error: RpcErrorBody }` from the Host
 * - event    `{ event, data }` from the Host, broadcast to every connected renderer
 */
import type { AssistantMessage, ImageContent, TextContent } from '@earendil-works/pi-ai'
import type { IdeExecutionRequest, IdeExecutionResponseMap } from './ide-execution-protocol.ts'
import type { IdeFilesFailure, IdeFilesRequest, IdeFilesResults, IdeStateRequest, WorkspaceId } from './ide-files-protocol.ts'
import type { RuntimeRequest, RuntimeSnapshot } from './runtime-protocol.ts'

export type { WorkspaceId }

/** Chat identifier: a UUID string. */
export type SessionId = string

// ───────────────────────────── Models and settings ─────────────────────────────

/** Wire protocols the Host can stream through pi-ai. */
export type ModelApi = 'openai-completions' | 'openai-responses' | 'anthropic-messages'
/** RainyAgent reasoning levels; each model maps them to its own wire values. */
export type ThinkingLevel = 'off' | 'low' | 'high' | 'max'

/** Saved model profile. Never carries a credential value. */
export interface ModelSetup {
  provider: string
  baseURL: string
  model: string
  contextWindow: number
  maxTokens?: number | undefined
  local: boolean
  api?: ModelApi | undefined
  thinking?: ThinkingLevel | undefined
  thinkingFormat?: 'openai' | 'deepseek' | 'qwen' | undefined
  maxTokensField?: 'max_tokens' | 'max_completion_tokens' | undefined
}
/** Model form submitted by Settings; `apiKey` is stored separately and never echoed. */
export type ModelSetupInput = ModelSetup & { apiKey?: string | undefined }
/** Connection fields required to list a provider's models. */
export interface ModelDiscoveryInput { provider: string; baseURL: string; api: ModelApi; apiKey?: string | undefined }
/** The model future chats start with. */
export interface ModelSelection { provider: string; model: string; thinking?: ThinkingLevel | undefined }

/** Model settings shown by Settings → Models and the composer's model picker. */
export interface ModelsStatus {
  models: ModelSetup[]
  /** Providers whose credential is stored. */
  credentials: string[]
  selected: ModelSelection | null
  /** Reasoning levels each saved model accepts, keyed by provider (one saved model per provider). */
  thinkingLevels: Record<string, ThinkingLevel[]>
  presets: { name: string; model: ModelSetup }[]
  globalPrompt: { text: string; maxChars: number }
}

/** Interface preferences kept by the Host so they survive renderer origin changes. */
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
}

/** An MCP server configured in Settings → Skills & MCP. */
export interface McpServerConfig {
  name: string
  enabled: boolean
  transport: 'stdio' | 'streamable-http'
  command?: string | undefined
  args?: string[] | undefined
  env?: Record<string, string> | undefined
  url?: string | undefined
  /** Request headers of a streamable-http server, such as `Authorization`. */
  headers?: Record<string, string> | undefined
  /** Tool names the model may call; an empty list allows every tool the server lists. */
  tools: string[]
}
/** Live state of one configured MCP server. */
export interface McpServerStatus {
  name: string
  state: 'disabled' | 'connecting' | 'ready' | 'error'
  error?: string | undefined
  tools: { name: string; description: string; enabled: boolean }[]
}
/** A discovered `SKILL.md`. */
export interface SkillSummary {
  id: string
  scope: 'project' | 'user'
  name: string
  description: string
  path: string
}
/** Settings → Skills & MCP view. */
export interface ExtensionsStatus {
  skills: SkillSummary[]
  servers: McpServerConfig[]
  status: McpServerStatus[]
  /** Path of the IDA MCP launcher (`uvx`) when the desktop found one. */
  idaAvailable: boolean
}

// ───────────────────────────── Chats ─────────────────────────────

/** Token usage of one model request or a whole turn. */
export interface UsageSummary {
  input: number
  output: number
  cacheRead: number
  cacheWrite: number
  reasoning?: number | undefined
  cost?: number | undefined
}

/** A transcript entry as persisted and as shown by the renderer. */
export type TranscriptEntry =
  | { id: string; kind: 'user'; ts: number; text: string; images?: ImageContent[] | undefined }
  | { id: string; kind: 'assistant'; ts: number; message: AssistantMessage; durationMs?: number | undefined }
  | {
    id: string; kind: 'toolResult'; ts: number; toolCallId: string; toolName: string
    content: (TextContent | ImageContent)[]; isError: boolean; details?: unknown; durationMs?: number | undefined
  }
  /** Text the Host added to the model context: workspace instructions, memory recall, referenced chats. */
  | { id: string; kind: 'context'; ts: number; label: 'instructions' | 'memory' | 'reference' | 'notice'; text: string }
  /** The entries from `firstId` through `lastId` are replaced in the model context by `summary`. */
  | { id: string; kind: 'compaction'; ts: number; firstId: string; lastId: string; summary: string; tokensBefore: number; trigger: 'auto' | 'overflow' | 'manual' }
  /** UI-only line: retries, errors, stopped runs. Never sent to the model. */
  | { id: string; kind: 'notice'; ts: number; level: 'info' | 'warning' | 'error'; text: string; code?: string | undefined }
  /** End-of-turn statistics. Never sent to the model. */
  | { id: string; kind: 'turn'; ts: number; durationMs: number; usage: UsageSummary; provider: string; model: string; requests: number }

/** History-list row. */
export interface SessionSummary {
  id: SessionId
  workspaceId: WorkspaceId | null
  cwd: string
  title: string
  createdAt: number
  updatedAt: number
  archived: boolean
  pinned: boolean
  status: SessionRunStatus
  parent?: { sessionId: SessionId; entryId: string } | undefined
}

export type SessionRunStatus = 'idle' | 'running' | 'compacting' | 'error'

/** A message waiting for the current run to reach a step boundary (steer) or to finish (queue). */
export interface QueuedMessage { id: string; text: string; mode: 'queue' | 'steer'; imageCount: number }

/** Context-window accounting for the next request. */
export interface ContextUsage {
  tokens: number
  contextWindow: number
  inputLimit: number
  outputTokens: number
  compactAt: number
  kind: 'exact' | 'estimated'
  breakdown: { system: number; tools: number; messages: number }
}

/** Everything the renderer needs to show one chat. */
export interface SessionSnapshot {
  summary: SessionSummary
  entries: TranscriptEntry[]
  model: ModelSelection | null
  queue: QueuedMessage[]
  context: ContextUsage | null
  /** Partial assistant output while a request is streaming. */
  streaming: AssistantMessage | null
  /** Tool calls currently executing. */
  runningTools: string[]
  error?: string | undefined
}

/** `@` completion row. */
export interface CompletionItem {
  kind: 'file' | 'directory' | 'session'
  /** Text inserted after `@`. */
  insert: string
  label: string
  detail?: string | undefined
}

/** Search hit over chat titles and text. */
export interface SessionSearchHit { summary: SessionSummary; snippet: string }

// ───────────────────────────── Project memory ─────────────────────────────

export interface ProjectMemoryItem {
  id: string
  text: string
  updatedAt?: string | undefined
  editedByUser?: boolean | undefined
  sources: { sessionId: string; seq: number; executionTargetId: string; file?: { path: string; version: string } | undefined }[]
}
export interface ProjectMemoryStatus {
  enabled: boolean
  generationEnabled: boolean
  revision: number
  updatedAt?: string | null | undefined
  items: ProjectMemoryItem[]
  pending?: boolean | undefined
  generating?: boolean | undefined
  error?: string | undefined
}

// ───────────────────────────── Budget preview ─────────────────────────────

export interface BudgetPreview {
  model: string
  tokens: number
  contextWindow: number
  inputLimit: number
  outputTokens: number
  marginTokens: number
  kind: 'exact' | 'estimated'
  breakdown: { system: number; tools: number; extensions: number; instructions: number; memory: number; history: number; framing: number }
}

// ───────────────────────────── App ─────────────────────────────

/** Values the Host also injects into `index.html` as `window.__RAINY_AGENT__`. */
export interface AppInfo {
  name: 'RainyAgent'
  version: string
  environment: 'Windows' | 'WSL'
  executionTargetId: string
  platform: 'win32' | 'linux'
  home: string
}

// ───────────────────────────── Method table ─────────────────────────────

/** IDE file, state and execution operations keep their 1.x request objects. */
export type IdeRequest = IdeFilesRequest | IdeStateRequest | IdeExecutionRequest | { op: 'format'; workspaceId: WorkspaceId; path: string; text: string; language: string }
/** Result type for one IDE operation. */
export type IdeResult<T extends IdeRequest> =
  T extends { op: keyof IdeFilesResults } ? IdeFilesResults[T['op']]
    : T extends { op: keyof IdeExecutionResponseMap } ? IdeExecutionResponseMap[T['op']]
      : T extends { op: 'format' } ? { text: string }
        : never

/** Every method the Host serves: `[params, result]`. */
export interface HostMethods {
  'app.info': [void, AppInfo]
  'prefs.get': [void, UiPreferences]
  'prefs.set': [Partial<UiPreferences>, UiPreferences]

  'models.status': [void, ModelsStatus]
  'models.configure': [ModelSetupInput, ModelSelection]
  'models.remove': [{ provider: string }, ModelsStatus]
  'models.select': [ModelSelection, ModelsStatus]
  'models.discover': [ModelDiscoveryInput, { id: string; contextWindow?: number | undefined }[]]
  'models.probe': [ModelSetupInput, { stream: boolean; toolCall: boolean; text?: string | undefined }]
  'prompt.global': [{ text: string }, ModelsStatus]
  'budget.preview': [{ workspaceId: WorkspaceId; sessionId?: SessionId | undefined; draft?: string | undefined }, BudgetPreview]

  'sessions.list': [{ workspaceId?: WorkspaceId | null | undefined; archived?: boolean | undefined }, SessionSummary[]]
  'sessions.search': [{ query: string; limit?: number | undefined }, SessionSearchHit[]]
  'sessions.create': [{ workspaceId: WorkspaceId | null; cwd?: string | undefined }, SessionSummary]
  'sessions.get': [{ sessionId: SessionId; limit?: number | undefined }, SessionSnapshot]
  'sessions.rename': [{ sessionId: SessionId; title: string }, SessionSummary]
  'sessions.archive': [{ sessionId: SessionId; archived: boolean }, SessionSummary]
  'sessions.pin': [{ sessionId: SessionId; pinned: boolean }, SessionSummary]
  'sessions.delete': [{ sessionId: SessionId }, void]
  'sessions.fork': [{ sessionId: SessionId; entryId: string }, SessionSummary]

  'chat.send': [{ sessionId: SessionId; text: string; images?: ImageContent[] | undefined; mode?: 'queue' | 'steer' | undefined }, { queued: boolean }]
  'chat.abort': [{ sessionId: SessionId }, void]
  'chat.unqueue': [{ sessionId: SessionId; id: string }, QueuedMessage[]]
  'chat.setModel': [{ sessionId: SessionId } & ModelSelection, ModelSelection]
  'chat.compact': [{ sessionId: SessionId }, { message: string }]
  'chat.complete': [{ sessionId?: SessionId | undefined; workspaceId?: WorkspaceId | null | undefined; query: string }, CompletionItem[]]

  'extensions.status': [{ cwd?: string | undefined }, ExtensionsStatus]
  'extensions.saveServer': [{ server: McpServerConfig; previousName?: string | undefined }, ExtensionsStatus]
  'extensions.removeServer': [{ name: string }, ExtensionsStatus]
  'extensions.addIda': [void, ExtensionsStatus]

  'memory.status': [{ workspaceId: WorkspaceId }, ProjectMemoryStatus]
  'memory.setEnabled': [{ workspaceId: WorkspaceId; enabled?: boolean | undefined; generationEnabled?: boolean | undefined }, ProjectMemoryStatus]
  'memory.edit': [{ workspaceId: WorkspaceId; id: string; text: string; expectedRevision: number }, ProjectMemoryStatus]
  'memory.delete': [{ workspaceId: WorkspaceId; id: string }, ProjectMemoryStatus]
  'memory.clear': [{ workspaceId: WorkspaceId }, ProjectMemoryStatus]

  'ide': [IdeRequest, unknown]
  'runtime': [RuntimeRequest, RuntimeSnapshot]
}

export type HostMethod = keyof HostMethods
export type MethodParams<M extends HostMethod> = HostMethods[M][0]
export type MethodResult<M extends HostMethod> = HostMethods[M][1]

/** Events the Host broadcasts. */
export interface HostEvents {
  /** A transcript entry was appended to a chat. */
  'session.entry': { sessionId: SessionId; entry: TranscriptEntry }
  /** Streaming progress of the current request; `message` is the partial assistant message. */
  'session.stream': { sessionId: SessionId; message: AssistantMessage | null }
  /** Partial output of a running tool. */
  'session.tool': { sessionId: SessionId; toolCallId: string; toolName: string; phase: 'start' | 'update' | 'end'; args?: unknown; partial?: string | undefined }
  /** Run status, queue or model changed. */
  'session.state': { sessionId: SessionId; status: SessionRunStatus; queue: QueuedMessage[]; model: ModelSelection | null; context: ContextUsage | null; error?: string | undefined }
  /** A chat was created or its summary changed. */
  'sessions.changed': SessionSummary
  'sessions.removed': { sessionId: SessionId }
  'models.changed': ModelsStatus
  'prefs.changed': UiPreferences
  'extensions.changed': ExtensionsStatus
  'memory.changed': { workspaceId: WorkspaceId }
}
export type HostEvent = keyof HostEvents

/** Error body of a failed request. `data` carries the IDE failure details when `code` is an IDE error code. */
export interface RpcErrorBody {
  code: string
  message: string
  data?: Partial<IdeFilesFailure> | undefined
}

export type RpcRequestFrame = { id: number; method: HostMethod; params: unknown }
export type RpcResponseFrame = { id: number; result?: unknown; error?: RpcErrorBody }
export type RpcEventFrame = { event: HostEvent; data: unknown }
