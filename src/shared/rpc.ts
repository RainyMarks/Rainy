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
import type { FileTailResult, FileUploadResult, IdeFilesRequest, IdeFilesResults, OfficePdfRegistration, WorkspaceId } from './ide-files-protocol.ts'
import type { FootprintTurn, MapExplanation, ProjectMap } from './map-protocol.ts'

export type { FileTailResult, FileUploadResult, OfficePdfRegistration, WorkspaceId }

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
  stopReason: 'stop' | 'length' | 'toolUse' | 'error' | 'aborted' | 'pending' | ''
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
  /** The optical look of the native interface (Settings → Appearance). */
  optics: boolean
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

/** Model selected for a chat or future chats (`rainy-protocol::models::ModelSelection`). */
export interface ModelSelection {
  provider: string
  model: string
  thinking?: string | null | undefined
}

/** Legacy/persisted model selection shape that may also carry `thinkingLevel` (`settings.json`). */
export interface StoredModelSelection extends ModelSelection {
  thinkingLevel?: string | null | undefined
}

/** External CLI agent backend attached to a chat (`codex`, `antigravity` or generic `acp`). */
export interface ExternalBackendRef {
  /** `codex`, `antigravity` or `acp`. */
  kind: string
  binary?: string | null | undefined
  args?: string[] | undefined
  label?: string | null | undefined
}

/** One model exposed by an external agent CLI (`external.status` / `external.probe`). */
export interface ExternalModelInfo {
  id: string
  displayName?: string | null | undefined
  reasoningEfforts?: string[] | undefined
  defaultEffort?: string | null | undefined
  isDefault?: boolean | null | undefined
}

/** User configuration for one external CLI agent stored in `settings.json` (`external.configure`). */
export interface ExternalAgentConfig {
  /** `codex`, `antigravity` or `acp`. */
  kind: string
  enabled?: boolean | undefined
  binary?: string | null | undefined
  args?: string[] | undefined
  label?: string | null | undefined
  defaultModel?: string | null | undefined
  defaultThinking?: string | null | undefined
}

/** Detection and configuration snapshot of one external agent (`external.status` / `external.probe`). */
export interface ExternalAgentStatus {
  kind: string
  label: string
  enabled: boolean
  installed: boolean
  binary?: string | null | undefined
  args?: string[] | undefined
  binaryPath?: string | null | undefined
  version?: string | null | undefined
  models?: ExternalModelInfo[] | undefined
  defaultModel?: string | null | undefined
  defaultThinking?: string | null | undefined
  installHint?: string | null | undefined
  loginHint?: string | null | undefined
  error?: string | null | undefined
  /** The slash commands the agent last listed (ACP `available_commands_update`). */
  commands?: ExternalCommand[] | undefined
  /** The account's usage limits as the agent last reported them (`external.quota`). */
  quota?: ExternalQuota | null | undefined
}

/** An external agent account's usage limits: Codex's five-hour and weekly windows, Antigravity's per model group. */
export interface ExternalQuota {
  /** Unix milliseconds when Rainy read them. */
  fetchedAt: number
  plan?: string | null | undefined
  windows?: QuotaWindow[] | undefined
  /** Why the limits could not be read, when they could not. */
  error?: string | null | undefined
}

/** One usage window of an external agent account. */
export interface QuotaWindow {
  /** The models the window counts ("Gemini Models"); absent for all of them. */
  group?: string | null | undefined
  /** The window's length in minutes: 300 is five hours, 10080 a week. */
  minutes?: number | null | undefined
  /** Percent of the window still available, 0–100. */
  remaining: number
  /** Unix seconds when the window starts over. */
  resetsAt?: number | null | undefined
}

/** A slash command an external agent offers, sent to it as `/name text`. */
export interface ExternalCommand {
  name: string
  description?: string | null | undefined
  /** What to write after it, when it takes text. */
  hint?: string | null | undefined
}

/** Response of `external.status`, `external.probe`, `external.configure` and payload of `external.changed`. */
export interface ExternalStatusResponse {
  agents: ExternalAgentStatus[]
}

/** A chat of the history list, as far as the project map reads it. */
export interface SessionSummary {
  id: SessionId
  /** The chat's working directory, which relative tool paths start from. */
  cwd: string
  /** External CLI agent backend driving this chat (`codex`, `antigravity` or `acp`), when set. */
  externalBackend?: ExternalBackendRef | null | undefined
  /** External thread or session identifier (`threadId` / ACP `sessionId`) used to resume across restarts. */
  externalThreadId?: string | null | undefined
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
  reason: 'command' | 'outside-write' | 'outside-read' | 'outside-workdir' | 'mcp' | 'web' | 'file-change' | 'permission'
  rule?: string | null | undefined
  path?: string | null | undefined
  workdir?: string | null | undefined
  /** The member of an external agent's team that asks (`@tester`), when not the agent itself. */
  agent?: string | null | undefined
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
  argv?: string[] | undefined
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
  breakpoints?: DebugBreakpoint[] | undefined
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

// ───────────────────────────── Host Metrics & Ports ─────────────────────────────
// Shared shapes mirrored with `native/crates/protocol/src/metrics.rs`.

/** One compute process currently occupying memory on a GPU. */
export interface GpuProcess {
  pid: number
  processName: string
  usedMemoryMib: number
  user?: string | null | undefined
  uid?: number | null | undefined
}

/** Instantaneous status of one NVIDIA GPU reported by `nvidia-smi`. */
export interface GpuInfo {
  index: number
  name: string
  utilizationPercent?: number | null | undefined
  memoryUsedMib: number
  memoryTotalMib: number
  temperatureC?: number | null | undefined
  processes?: GpuProcess[] | undefined
}

/** Container cgroup v1/v2 CPU and memory limits, when enforced on Linux. */
export interface CgroupLimits {
  /** `1` for cgroup v1, `2` for cgroup v2. */
  version: number
  cpuQuotaCores?: number | null | undefined
  memoryLimitBytes?: number | null | undefined
  memoryCurrentBytes?: number | null | undefined
}

/** Available and total filesystem space for one watched path or mount point. */
export interface DiskUsage {
  path: string
  mount?: string | null | undefined
  availableBytes: number
  totalBytes: number
}

/** Response of `host.metrics`: lightweight CPU, memory, cgroup, GPU and disk snapshot. */
export interface HostMetrics {
  collectedAt: number
  hostname: string
  platform: string
  cpuCount: number
  effectiveCpuCores: number
  cpuPercent?: number | null | undefined
  loadAvg1m?: number | null | undefined
  memoryUsedBytes: number
  memoryTotalBytes: number
  hostMemoryTotalBytes: number
  cgroup?: CgroupLimits | null | undefined
  gpus?: GpuInfo[] | undefined
  disks?: DiskUsage[] | undefined
}

/** One TCP port currently listening on the Host (`host.ports`), used to offer one-click SSH `-L` forwarding. */
export interface ListeningPort {
  port: number
  address: string
  pid?: number | null | undefined
  processName?: string | null | undefined
  label?: string | null | undefined
}

/** Response of `host.ports`. */
export interface HostPortsResponse {
  ports: ListeningPort[]
}

// ───────────────────────────── Detached Experiments ─────────────────────────────
// Shared shapes mirrored with `native/crates/protocol/src/experiments.rs`.

/** Lifecycle state of a detached background experiment. */
export type ExperimentStatus = 'running' | 'exited' | 'stopped' | 'unknown'

/** One persisted detached experiment entry (`experiments.list`, `experiments.start`, `experiments.stop`). */
export interface ExperimentRecord {
  id: string
  name: string
  command: string
  cwd: string
  logPath: string
  status: ExperimentStatus
  pid?: number | null | undefined
  pgid?: number | null | undefined
  procStartTicks?: number | null | undefined
  uid?: number | null | undefined
  cudaVisibleDevices?: string | null | undefined
  startedAt: number
  endedAt?: number | null | undefined
  exitCode?: number | null | undefined
}

/** Request payload for `experiments.start`. */
export interface ExperimentStartParams {
  name?: string | null | undefined
  cwd: string
  command: string
  cudaVisibleDevices?: string | null | undefined
  env?: Record<string, string> | undefined
  logPath?: string | null | undefined
}

/** Incremental log tail chunk returned by `experiments.tail`. */
export interface ExperimentTailResult {
  id: string
  logPath: string
  text: string
  offset: number
  nextOffset: number
  totalBytes: number
  truncated: boolean
  reset: boolean
}

/** Response of `experiments.list`. */
export interface ExperimentListResponse {
  experiments: ExperimentRecord[]
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
  /** Create a chat, optionally bound to an external CLI agent backend. */
  'sessions.create': [{ workspaceId?: WorkspaceId | null | undefined; cwd?: string | undefined; permission?: 'read-only' | 'ask' | 'auto' | undefined; worktree?: boolean | undefined; model?: ModelSelection | null | undefined; externalBackend?: ExternalBackendRef | null | undefined }, SessionSummary]
  /** Files the chat's agent changed that are not yet kept or undone. */
  'chat.changes': [{ sessionId: SessionId }, ChatChange[]]
  /** Detailed token usage breakdown for a chat's next request. */
  'session.contextBreakdown': [{ sessionId: SessionId }, ContextBreakdown]
  /** Configuration and cached probe status of external CLI agents. */
  'external.status': [Record<string, never>, ExternalStatusResponse]
  /** Read an external agent's usage limits again (Codex, Antigravity); a reading under a minute old is kept unless `force`. */
  'external.quota': [{ kind: string; force?: boolean | undefined; cwd?: string | undefined }, ExternalStatusResponse]
  /** Probe external CLI binaries and models on demand. */
  'external.probe': [{ kind?: string | undefined; binary?: string | undefined; args?: string[] | undefined; cwd?: string | undefined }, ExternalStatusResponse]
  /** Update or remove one external CLI agent entry in `settings.json`. */
  'external.configure': [{ config: ExternalAgentConfig; remove?: boolean | undefined }, ExternalStatusResponse]
  /** Lightweight CPU, memory, cgroup, GPU and disk metrics snapshot. */
  'host.metrics': [{ watchPaths?: string[] | undefined; includeGpuProcesses?: boolean | undefined }, HostMetrics]
  /** TCP ports currently listening on the Host for one-click SSH forwarding. */
  'host.ports': [Record<string, never>, HostPortsResponse]
  /** Detached background experiments registered on the Host. */
  'experiments.list': [Record<string, never>, ExperimentListResponse]
  /** Start a detached background experiment that outlives Host restarts. */
  'experiments.start': [ExperimentStartParams, ExperimentRecord]
  /** Stop a detached background experiment after verifying its process identity. */
  'experiments.stop': [{ id: string; force?: boolean | undefined }, ExperimentRecord]
  /** Remove a detached experiment entry from the Host registry. */
  'experiments.remove': [{ id: string }, { id: string; removed: boolean }]
  /** Read an incremental log tail slice for one detached experiment. */
  'experiments.tail': [{ id: string; offset?: number | undefined; maxBytes?: number | undefined }, ExperimentTailResult]
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
  'session.tool': { sessionId: SessionId; toolCallId: string; toolName: string; phase: 'start' | 'update' | 'end'; args?: unknown; partial?: string | undefined; details?: unknown }
  /** Run status, queue or model changed. */
  'session.state': { sessionId: SessionId; status: SessionRunStatus }
  /** A chat was created or its summary changed. */
  'sessions.changed': SessionSummary
  /** External CLI agent configuration or probe status changed. */
  'external.changed': ExternalStatusResponse
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
