/** Settings HTTP validation and retained status; forms retain their own unsaved fields. */
import { z } from 'zod'
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import { budgetPreviewSchema, settingsStatusSchema, type SettingsOperations, type SettingsStatus, type RuntimeNativeHost } from './settings-protocol.ts'

/** Status loading and failures are separate from user-edited form fields. */
export interface SettingsSnapshot { status?: SettingsStatus | undefined; loading: boolean; error: string }

async function control<T>(method: string, params: unknown, schema: z.ZodType<T>): Promise<T> {
  const response = await fetch('/rainy/control', { method: 'POST', credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ method, params }) })
  const value: unknown = await response.json()
  const envelope = z.object({ result: z.unknown().optional(), error: z.string().optional() }).parse(value)
  if (!response.ok || envelope.error !== undefined) throw new Error(envelope.error ?? `HTTP ${response.status}`)
  return schema.parse(envelope.result)
}

const memorySchema = z.object({ enabled: z.boolean(), generationEnabled: z.boolean(), revision: z.number(),
  updatedAt: z.string().nullable().optional(),
  items: z.array(z.object({ id: z.string(), text: z.string(), updatedAt: z.string().optional(), editedByUser: z.boolean().optional(),
    sources: z.array(z.object({ sessionId: z.string(), seq: z.number(), executionTargetId: z.string(),
      file: z.object({ path: z.string(), version: z.string() }).optional() })) })),
  pending: z.boolean().optional(), generating: z.boolean().optional(), error: z.string().optional(), usage: z.unknown().optional(),
})

/** Own shared status requests and release pending reads when its UI plugin unmounts. */
export class SettingsController {
  readonly state = createSnapshotStore<SettingsSnapshot>({ loading: false, error: '' })
  readonly operations: SettingsOperations
  private pending: Promise<void> | undefined
  private controller = new AbortController()

  constructor() {
    const host = window as typeof window & { __RAINY_RUNTIME_NATIVE__?: RuntimeNativeHost }
    this.operations = {
      previewBudget: request => control('preview-budget', request, budgetPreviewSchema),
      refresh: () => this.refresh(),
      configure: async (setup) => {
        const configured = await control('configure-model', setup, z.object({ provider: z.string(), model: z.string() }))
        await this.refreshAfterChange()
        return configured
      },
      saveGlobalPrompt: async (text) => {
        await control('configure-global-prompt', { text }, z.unknown())
        await this.refreshAfterChange()
      },
      discover: async setup => (await control('discover-models', setup,
        z.object({ data: z.array(z.object({ id: z.string(), contextWindow: z.number().optional() })).optional() }))).data ?? [],
      probe: setup => control('probe-model', setup, z.object({ stream: z.boolean(), toolCall: z.boolean(), text: z.string().optional() })),
      catalog: sessionId => control('extensions-catalog', sessionId,
        z.object({ skills: z.array(z.object({ id: z.string() })), selection: z.unknown(), idaAvailable: z.boolean() })),
      extensions: async (sessionId, selection) => { await control('extensions-select', { sessionId, selection }, z.unknown()) },
      memory: workspaceId => control('project-memory-status', { workspaceId }, memorySchema),
      memoryEnabled: async (workspaceId, values) => { await control('project-memory-set-enabled', { workspaceId, ...values }, z.unknown()) },
      memoryEdit: async (workspaceId, id, text, expectedRevision) => { await control('project-memory-edit', { workspaceId, id, text, expectedRevision }, z.unknown()) },
      memoryDelete: async (workspaceId, id) => { await control('project-memory-delete', { workspaceId, id }, z.unknown()) },
      memoryClear: async (workspaceId) => { await control('project-memory-clear', { workspaceId }, z.unknown()) },
      runtime: async (request) => {
        const response = await fetch('/rainy/runtime', { method: 'POST', credentials: 'same-origin',
          headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(request) })
        const envelope: unknown = await response.json()
        const result = runtimeEnvelopeSchema.parse(envelope)
        if (!result.ok) throw new Error(typeof result.error === 'string' ? result.error : result.error.message)
        return result.result
      },
      runtimeNative: host.__RAINY_RUNTIME_NATIVE__,
    }
  }

  /** Read the latest redacted status once even when multiple sections request it.
   * @returns Completion after publishing the response or its diagnostic.
   */
  refresh(): Promise<void> {
    if (this.pending !== undefined) return this.pending
    this.state.set({ ...this.state.getSnapshot(), loading: true, error: '' })
    this.pending = fetch('/rainy/control', { credentials: 'same-origin', signal: this.controller.signal })
      .then(async (response) => {
        if (!response.ok) throw new Error(`HTTP ${response.status}`)
        const status = settingsStatusSchema.parse(await response.json())
        if (!this.controller.signal.aborted) this.state.set({ status, loading: false, error: '' })
      }).catch((error: unknown) => {
        if (!this.controller.signal.aborted) this.state.set({ ...this.state.getSnapshot(), loading: false,
          error: error instanceof Error ? error.message : String(error) })
      }).finally(() => { this.pending = undefined })
    return this.pending
  }

  /** Read after a saved change, even when an earlier status request is still pending.
   * @returns Completion after a fresh status read or plugin disposal.
   */
  async refreshAfterChange(): Promise<void> {
    await this.pending
    if (!this.controller.signal.aborted) await this.refresh()
  }

  /** Abort the controller's read request; submitted user changes retain their own Host lifetime. */
  dispose(): void { this.controller.abort() }
}

const language = z.enum(['python', 'node', 'php', 'c', 'cpp'])
const candidate = z.object({ id: z.string().transform(value => value as import('../runtime-protocol.ts').RuntimeEnvironmentId),
  language, path: z.string(), source: z.enum(['system', 'project', 'conda', 'bundled', 'manual']), platform: z.enum(['windows', 'linux']),
  version: z.string().nullable(), prefix: z.string().optional(), ready: z.boolean(), error: z.string().optional(),
  capabilities: z.array(z.object({ name: z.string(), ready: z.boolean(), detail: z.string().optional() })),
})
const runtimeEnvelopeSchema = z.discriminatedUnion('ok', [
  z.object({ ok: z.literal(true), result: z.object({
    targetId: z.string().transform(value => value as import('../runtime-protocol.ts').ExecutionTargetId),
    workspaceId: z.string().transform(value => value as import('../ide-files-protocol.ts').WorkspaceId),
    platform: z.enum(['windows', 'linux']), candidates: z.array(candidate), selected: z.partialRecord(language, candidate),
  }) }),
  z.object({ ok: z.literal(false), error: z.union([z.string(), z.object({ message: z.string() })]) }),
])
