/** Validated settings responses shared by the Rainy settings controller and its sections. */
import { z } from 'zod'
import type { WorkspaceId } from '../ide-files-protocol.ts'
import type { RuntimeRequest, RuntimeSnapshot } from '../runtime-protocol.ts'

/** Saved model metadata never includes a credential value. */
export const modelSchema = z.object({
  provider: z.string(), baseURL: z.string(), model: z.string(), contextWindow: z.number(), maxTokens: z.number().optional(),
  local: z.boolean(), api: z.enum(['openai-completions', 'openai-responses', 'anthropic-messages']).optional(),
  thinking: z.enum(['off', 'low', 'high', 'max']).optional(),
  thinkingFormat: z.enum(['openai', 'deepseek', 'qwen']).optional(), maxTokensField: z.enum(['max_tokens', 'max_completion_tokens']).optional(),
})
/** Model form accepted by the existing configuration endpoint. */
export type RainyModelSetup = z.infer<typeof modelSchema> & { apiKey?: string | undefined }
/** Connection fields required to list models before a model is selected. */
export type RainyModelDiscovery = Pick<RainyModelSetup, 'provider' | 'baseURL' | 'apiKey'> & {
  api: NonNullable<RainyModelSetup['api']>
}
/** Request accounting deliberately contains no prompt or credential text. */
export const budgetSchema = z.object({ sessionId: z.string(), model: z.string(), tokens: z.number(), contextWindow: z.number(),
  inputLimit: z.number(), outputTokens: z.number(), marginTokens: z.number(), kind: z.enum(['exact', 'estimated']),
  compacting: z.boolean(), error: z.string().optional(),
  breakdown: z.object({ system: z.number(), tools: z.number(), extensions: z.number(), instructions: z.number(),
    memory: z.number(), history: z.number(), framing: z.number() }).optional(),
})
/** Before-dispatch estimate whose limitations stay explicit in the UI. */
export const budgetPreviewSchema = budgetSchema.extend({ preview: z.literal(true),
  limitations: z.array(z.enum(['before-dispatch-estimate', 'attachments-not-included'])) })
/** Current project estimate without creating a conversation or invoking a model. */
export type BudgetPreview = z.infer<typeof budgetPreviewSchema>
/** Current model configuration and active conversation inventory. */
export const settingsStatusSchema = z.object({
  models: z.array(modelSchema), budgets: z.array(budgetSchema),
  selected: z.object({ provider: z.string(), model: z.string() }).nullish(),
  sessions: z.array(z.object({ id: z.string(), title: z.string().optional(), status: z.string() })),
  presets: z.array(z.object({ name: z.string(), model: modelSchema })).optional(), tools: z.array(z.string()),
  globalPrompt: z.object({ text: z.string(), maxChars: z.number() }),
})
/** Settings read state supplied through the framework hook. */
export type SettingsStatus = z.infer<typeof settingsStatusSchema>

/** Per-project memory metadata exposed by the authenticated Host. */
export interface ProjectMemoryStatus {
  enabled: boolean
  generationEnabled: boolean
  revision: number
  updatedAt?: string | null | undefined
  items: readonly {
    id: string
    text: string
    updatedAt?: string | undefined
    editedByUser?: boolean | undefined
    sources: readonly {
      sessionId: string
      seq: number
      executionTargetId: string
      file?: { path: string; version: string } | undefined
    }[]
  }[]
  pending?: boolean | undefined
  generating?: boolean | undefined
  error?: string | undefined
  usage?: unknown
}

/** A carrier-owned execution target. */
export interface RuntimeNativeTarget { id: string; kind: 'windows' | 'wsl'; label: string; distro?: string }
/** Context-isolated execution target navigation and component preparation. */
export interface RuntimeNativeHost {
  targets(): Promise<{ current: RuntimeNativeTarget; targets: RuntimeNativeTarget[]; projectId?: string }>
  switchTarget(request: { targetId: string; workspaceId?: WorkspaceId | undefined }): Promise<{ ok: boolean; error?: string }>
  prepare(): Promise<void>
  /** @param listener Current component preparation message. @returns A disposer for the native subscription. */
  onProgress(listener: (message: string) => void): () => void
}

/** Typed UI operations keep HTTP and native bridges outside React sections. */
export interface SettingsOperations {
  previewBudget(request: {
    workspaceId: WorkspaceId
    sessionId?: string | undefined
    provider?: string | undefined
    model?: string | undefined
    draft?: string | undefined
  }): Promise<BudgetPreview>
  refresh(): Promise<void>
  configure(setup: RainyModelSetup): Promise<{ provider: string; model: string }>
  /** Save instructions added to every later model request; resolves after the status refresh. */
  saveGlobalPrompt(text: string): Promise<void>
  discover(connection: RainyModelDiscovery): Promise<readonly { id: string; contextWindow?: number | undefined }[]>
  probe(setup: RainyModelSetup): Promise<{ stream: boolean; toolCall: boolean; text?: string | undefined }>
  catalog(sessionId: string): Promise<{ skills: readonly { id: string }[]; selection: unknown; idaAvailable: boolean }>
  extensions(sessionId: string, selection: unknown): Promise<void>
  memory(workspaceId: WorkspaceId): Promise<ProjectMemoryStatus>
  memoryEnabled(workspaceId: WorkspaceId, values: { enabled?: boolean | undefined; generationEnabled?: boolean | undefined }): Promise<void>
  memoryEdit(workspaceId: WorkspaceId, id: string, text: string, expectedRevision: number): Promise<void>
  memoryDelete(workspaceId: WorkspaceId, id: string): Promise<void>
  memoryClear(workspaceId: WorkspaceId): Promise<void>
  runtime(request: RuntimeRequest): Promise<RuntimeSnapshot>
  readonly runtimeNative?: RuntimeNativeHost | undefined
}
