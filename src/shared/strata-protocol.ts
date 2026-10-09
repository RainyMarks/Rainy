/** Native desktop controls for the bundled Strata runtime and user-selected external model files. */
import { z } from 'zod'

/** Engine allocation and model locations; request reasoning and output limits remain model settings. */
export const strataSettingsSchema = z.object({
  modelPath: z.string().max(32768),
  mtpPath: z.string().max(32768),
  contextWindow: z.number().int().min(8192).max(262144),
  port: z.number().int().min(1024).max(65535),
  kvCache: z.enum(['int8', 'q4_0', 'k8v4']),
  vramReserveMiB: z.number().int().min(0),
  residentBudgetGiB: z.number().positive().nullable(),
}).strict()
/** Persisted controls for a supported Qwen3.8 Flash Next model. */
export type StrataSettings = z.infer<typeof strataSettingsSchema>

/** Model files remain outside the application runtime and are never downloaded by this integration. */
export const strataModelSchema = z.object({
  sourcePath: z.string(),
  model: z.string(),
  ggufPath: z.string(),
  packPath: z.string().nullable(),
  tokenizerPath: z.string().nullable(),
  mtpPath: z.string().nullable(),
  needsPreparation: z.boolean(),
}).strict()
/** Inspected external files needed by the bundled engine. */
export type StrataModel = z.infer<typeof strataModelSchema>

/** Live state contains public health metadata, never API keys, prompts, or raw process logs. */
export const strataStatusSchema = z.object({
  phase: z.enum(['unconfigured', 'stopped', 'preparing', 'starting', 'running', 'stopping', 'external', 'error']),
  settings: strataSettingsSchema,
  runtime: z.object({ available: z.boolean(), version: z.string().nullable(), root: z.string(), missing: z.array(z.string()) }).strict(),
  profiles: z.array(z.object({ path: z.string(), label: z.string() }).strict()),
  model: strataModelSchema.nullable(),
  server: z.object({
    baseURL: z.string(), model: z.string(), contextWindow: z.number().int().positive(),
    loaded: z.boolean(), owned: z.boolean(), authenticationRequired: z.boolean(),
  }).strict().nullable(),
  progress: z.string().nullable(),
  error: z.string().nullable(),
}).strict()
/** Snapshot shared by native IPC and the settings card. */
export type StrataStatus = z.infer<typeof strataStatusSchema>

/** A loaded local server still requires an identity probe from the currently selected Rainy Host. */
export const strataConnectionSchema = z.object({
  baseURL: z.string(), model: z.string(), contextWindow: z.number().int().positive(),
}).strict()
/** Credential-free connection descriptor accepted by the authenticated Host. */
export type StrataConnection = z.infer<typeof strataConnectionSchema>

/** Native picker intent; MTP accepts a matching GGUF or a prepared directory. */
export type StrataModelPicker = 'gguf' | 'mtp' | 'directory' | 'profile'

/** Context-isolated carrier controls; only an explicit start operation may allocate model resources. */
export interface StrataNativeHost {
  /** @returns current model, bundled runtime, and public server health. */
  status(): Promise<StrataStatus>
  /** @param settings - edited engine controls and external model source. @returns persisted settings and inspected files. */
  save(settings: StrataSettings): Promise<StrataStatus>
  /** @returns promptly after preparation or startup is admitted; poll status until the model is ready. */
  start(): Promise<StrataStatus>
  /** @returns after owned preparation and server processes have exited; external servers remain untouched. */
  stop(): Promise<StrataStatus>
  /** @param kind - native file or directory picker. @returns the chosen path, or null after cancellation. */
  selectModel(kind: StrataModelPicker): Promise<string | null>
  /** @returns the saved provider and model after the selected Host verifies the loaded local endpoint. */
  connect(): Promise<{ provider: string; model: string }>
}
