/**
 * Host ↔ renderer protocol carried by the `/rpc` WebSocket.
 *
 * Frames are JSON objects:
 * - request  `{ id, method, params }` from the renderer
 * - response `{ id, result }` or `{ id, error: RpcErrorBody }` from the Host
 * - event    `{ event, data }` from the Host, broadcast to every connected renderer
 */
import type { AssistantMessage, ImageContent, TextContent } from '@earendil-works/pi-ai'
import type { Accent, ColorScheme } from './appearance.ts'
import type { IdeExecutionRequest, IdeExecutionResponseMap } from './ide-execution-protocol.ts'
import type { IdeFilesFailure, IdeFilesRequest, IdeFilesResults, IdeRootId, IdeStateRequest, WorkspaceId } from './ide-files-protocol.ts'
import type { GitStatus } from './git-protocol.ts'
import type { MapExplanation, ProjectMap } from './map-protocol.ts'
import type { TextReplaceRequest, TextReplaceResult, TextSearchRequest, TextSearchResult } from './text-search.ts'
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
export type ModelSetupInput = ModelSetup & {
  apiKey?: string | undefined
  /** The provider id the edited profile was saved under; a different `provider` moves the profile instead of adding one. */
  previousProvider?: string | undefined
}
/** Connection fields required to list a provider's models. */
export interface ModelDiscoveryInput { provider: string; baseURL: string; api: ModelApi; apiKey?: string | undefined }
/** The model future chats start with. */
export interface ModelSelection { provider: string; model: string; thinking?: ThinkingLevel | undefined }

/** Model settings shown by Settings → Models and the composer's model picker. */
export interface ModelsStatus {
  models: ModelSetup[]
  /** Providers of saved models and presets whose credential is stored. */
  credentials: string[]
  /** Whether stored API keys are encrypted with Windows data protection; absent from Hosts before 2.1. */
  keysEncrypted?: boolean
  /** Which stored keys this Windows account can no longer decrypt, in the interface language; absent when all can. */
  keysUnreadable?: string | undefined
  selected: ModelSelection | null
  /** Reasoning levels each saved model accepts, keyed by provider (one saved model per provider). */
  thinkingLevels: Record<string, ThinkingLevel[]>
  presets: { name: string; model: ModelSetup }[]
  globalPrompt: { text: string; maxChars: number }
}

/** How requests to model services and MCP servers reach the network. */
export interface ProxySettings {
  /** `system`: the system proxy (Windows settings, or HTTPS_PROXY inside WSL); `off`: direct; `manual`: `url`. */
  mode: 'system' | 'off' | 'manual'
  /** `host:port` or an `http://` URL, for `manual`. */
  url?: string | undefined
}
/** Settings → Models network view. Passwords in proxy URLs are masked. */
export interface ProxyStatus {
  settings: ProxySettings
  /** The proxy remote requests use now, or `null` for direct. */
  effective: string | null
  /** The system proxy as this Host sees it. */
  system: string | null
}
/** Outcome of reaching the selected model's service through some proxy settings. */
export interface ProxyTestResult { ok: boolean; url: string; status?: number | undefined; ms: number; error?: string | undefined }

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
  /** Chats get show_widget, which answers with an interactive page in a sandboxed frame. */
  widgets: boolean
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
  | {
    id: string; kind: 'assistant'; ts: number; message: AssistantMessage; durationMs?: number | undefined
    /** How long each thinking block took to stream, by content index; absent in chats written before 2.4. */
    thinkingMs?: Record<number, number> | undefined
  }
  | {
    id: string; kind: 'toolResult'; ts: number; toolCallId: string; toolName: string
    content: (TextContent | ImageContent)[]; isError: boolean; details?: unknown; durationMs?: number | undefined
  }
  /**
   * Text the Host added to the model context: workspace instructions, memory recall, referenced chats, notices.
   * `reverted` lists the write and edit calls whose changes the user undid; the notice tells the model.
   */
  | { id: string; kind: 'context'; ts: number; label: 'instructions' | 'memory' | 'reference' | 'notice'; text: string; reverted?: string[] | undefined }
  /**
   * The entries from `firstId` through `lastId` are replaced in the model context by `summary`. `request` says whether
   * the summary came from a request that shared the chat's prompt cache, and what the summary requests used.
   */
  | {
    id: string; kind: 'compaction'; ts: number; firstId: string; lastId: string; summary: string; tokensBefore: number; trigger: 'auto' | 'overflow' | 'manual'
    request?: { mode: 'shared' | 'separate'; usage: UsageSummary } | undefined
  }
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
  /** A tool call is waiting for the user's approval. */
  awaitingApproval?: boolean | undefined
}

export type SessionRunStatus = 'idle' | 'running' | 'compacting' | 'error'

/**
 * What the agent may do without asking. `read-only`: read and search only. `ask`: reads, searches and changes inside the
 * project run; commands, MCP tools and writes outside the project wait for approval. `auto`: everything runs.
 */
export type PermissionMode = 'read-only' | 'ask' | 'auto'

/** A tool call waiting for the user. */
export interface PendingApproval {
  toolCallId: string
  toolName: string
  /** Why it waits: a command, a write outside the project, an MCP tool, or a web page from a site not yet allowed. */
  reason: 'command' | 'outside-write' | 'mcp' | 'web'
  /** The command prefix, MCP tool or `web_fetch:<host>` "always allow" would remember; absent when the call can only be allowed once. */
  rule?: string | undefined
  /** Absolute target of a write outside the project, or the URL of a web page. */
  path?: string | undefined
}

/** The user's answer to a pending approval. */
export type ApprovalDecision = 'allow' | 'always' | 'deny'

/** Calls the user lets the agent make without asking, per project. */
export interface PermissionRules {
  /** Command prefixes such as `python3` or `git status`; they match simple commands only. */
  commands: string[]
  /** MCP tool names such as `mcp__files__read_file`, and `web_fetch:<host>` for sites web_fetch may read. */
  tools: string[]
}

/** Settings → Security view for one project. */
export interface SecurityStatus {
  /** Mode new chats start in. */
  defaultMode: PermissionMode
  rules: PermissionRules
}

/** Outcome of undoing write and edit calls. */
export interface RevertResult {
  reverted: { toolCallId: string; path: string }[]
  failed: { toolCallId: string; path: string; reason: 'changed' | 'unavailable' | 'failed'; message: string }[]
}

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
  /** What the agent may do without asking in this chat. */
  permission: PermissionMode
  /** Tool calls waiting for the user's approval. */
  approvals: PendingApproval[]
  /** Set while the run waits to retry a request that failed for a passing reason. */
  retry?: RetryWait | undefined
  error?: string | undefined
}

/** A run waiting before it automatically retries a request the provider refused or dropped for a passing reason (overload, rate limit, network). */
export interface RetryWait {
  /** 1 for the first retry. */
  attempt: number
  /** Automatic retries allowed before the run gives up. */
  max: number
  /** When the retry starts (epoch ms). */
  at: number
  /** The provider's error text. */
  reason: string
}

/** Text around the editor cursor for a ghost-text suggestion. */
export interface InlineCompletionRequest {
  /** Workspace-relative path, shown to the model. */
  path: string
  language?: string | undefined
  /** Text before the cursor, ending at it. */
  prefix: string
  /** Text after the cursor. */
  suffix: string
}

/** A suggestion, or why there is none; an empty `text` with no `error` means nothing fits. */
export interface InlineCompletionResult {
  text: string
  error?: string | undefined
}

/** A selection to rewrite for an instruction (Ctrl+K in the editor). */
export interface InlineEditRequest {
  /** Names the request for `inline.progress` and `inline.cancel`. */
  requestId: string
  path: string
  language?: string | undefined
  /** Text before the selection. */
  before: string
  /** Selected text; empty to insert at the cursor. */
  selection: string
  /** Text after the selection. */
  after: string
  instruction: string
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
  name: 'Rainy'
  version: string
  environment: 'Windows' | 'WSL' | 'SSH'
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
  'network.status': [void, ProxyStatus]
  'network.set': [ProxySettings, ProxyStatus]
  'network.test': [ProxySettings, ProxyTestResult]
  'budget.preview': [{ workspaceId: WorkspaceId; sessionId?: SessionId | undefined; draft?: string | undefined }, BudgetPreview]

  'sessions.list': [{ workspaceId?: WorkspaceId | null | undefined; archived?: boolean | undefined }, SessionSummary[]]
  'sessions.search': [{ query: string; limit?: number | undefined }, SessionSearchHit[]]
  'sessions.create': [{ workspaceId: WorkspaceId | null; cwd?: string | undefined; permission?: PermissionMode | undefined }, SessionSummary]
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
  /** Ghost-text suggestion at the editor cursor; a newer request ends the previous one. */
  'inline.complete': [InlineCompletionRequest, InlineCompletionResult]
  /** Replacement text for an editor selection; progress arrives as `inline.progress`. */
  'inline.edit': [InlineEditRequest, { text: string }]
  /** Stop an `inline.edit` request. */
  'inline.cancel': [{ requestId: string }, void]
  'chat.setPermission': [{ sessionId: SessionId; mode: PermissionMode }, PermissionMode]
  'chat.approve': [{ sessionId: SessionId; toolCallId: string; decision: ApprovalDecision; reason?: string | undefined }, void]
  /**
   * Request the model's answer again after a failed or stopped request, from the chat as it is; while an automatic
   * retry is waiting, retry now.
   */
  'chat.retry': [{ sessionId: SessionId }, void]
  /** Undo write and edit calls of the chat, newest first; files changed since are left alone. */
  'chat.revert': [{ sessionId: SessionId; toolCallIds: string[] }, RevertResult]
  /** Files the chat's agent changed that are not yet kept or undone. */
  'chat.changes': [{ sessionId: SessionId }, ChatChange[]]
  /** A changed file's text before the chat's changes and now. */
  'chat.changeBase': [{ sessionId: SessionId; path: string }, { base: string; current: string | null }]
  /** Mark write and edit calls as reviewed and kept. */
  'chat.keep': [{ sessionId: SessionId; toolCallIds: string[] }, void]

  /** The project map; a recent one is reused unless `refresh` is set. */
  'map.get': [{ workspaceId: WorkspaceId; refresh?: boolean | undefined }, ProjectMap]
  /** Ask the chat model to describe the project's parts; the answer is kept with the project. */
  'map.explain': [{ workspaceId: WorkspaceId }, MapExplanation]

  /** Search every root of a project; a newer search of the same project ends the older one. */
  'search.text': [TextSearchRequest, TextSearchResult]
  /** Replace every match in the listed files on disk. */
  'search.replace': [TextReplaceRequest, TextReplaceResult]

  'git.status': [{ workspaceId: WorkspaceId }, GitStatus]
  'git.stage': [{ workspaceId: WorkspaceId; rootId: IdeRootId; paths: string[] }, void]
  'git.unstage': [{ workspaceId: WorkspaceId; rootId: IdeRootId; paths: string[] }, void]
  /** Throw away working-tree changes; new files are deleted. */
  'git.discard': [{ workspaceId: WorkspaceId; rootId: IdeRootId; paths: string[] }, void]
  'git.commit': [{ workspaceId: WorkspaceId; rootId: IdeRootId; message: string; stageAll?: boolean | undefined }, { commit: string }]
  'git.init': [{ workspaceId: WorkspaceId; rootId: IdeRootId }, void]
  /** A commit message for the staged changes (or all of them when nothing is staged), written by the chat model. */
  'git.message': [{ workspaceId: WorkspaceId; rootId: IdeRootId }, { message: string }]

  'security.status': [{ workspaceId?: WorkspaceId | null | undefined }, SecurityStatus]
  'security.setDefault': [{ mode: PermissionMode }, SecurityStatus]
  'security.removeRule': [{ workspaceId: WorkspaceId; kind: 'command' | 'tool'; rule: string }, SecurityStatus]

  'extensions.status': [{ cwd?: string | undefined }, ExtensionsStatus]
  'extensions.saveServer': [{ server: McpServerConfig; previousName?: string | undefined }, ExtensionsStatus]
  'extensions.removeServer': [{ name: string }, ExtensionsStatus]

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
  /** The chat's list of changed files may have changed: a write or edit finished, or changes were kept or undone. */
  'session.changes': { sessionId: SessionId }
  /** Partial output of a running tool. */
  'session.tool': { sessionId: SessionId; toolCallId: string; toolName: string; phase: 'start' | 'update' | 'end'; args?: unknown; partial?: string | undefined }
  /** Run status, queue or model changed. */
  'session.state': {
    sessionId: SessionId; status: SessionRunStatus; queue: QueuedMessage[]; model: ModelSelection | null; context: ContextUsage | null
    permission: PermissionMode; approvals: PendingApproval[]; retry?: RetryWait | undefined; error?: string | undefined
  }
  /** A chat was created or its summary changed. */
  'sessions.changed': SessionSummary
  'sessions.removed': { sessionId: SessionId }
  'models.changed': ModelsStatus
  /** The default AI permission for new chats changed. */
  'security.changed': { defaultMode: PermissionMode }
  'prefs.changed': UiPreferences
  'extensions.changed': ExtensionsStatus
  'memory.changed': { workspaceId: WorkspaceId }
  /** A file of an open editor tab or a run of the workspace changed; the page polls now instead of waiting. */
  'ide.hint': { workspaceId: WorkspaceId; kind: 'files' | 'execution' }
  /** Replacement text an `inline.edit` request has produced so far. */
  'inline.progress': { requestId: string; text: string }
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
