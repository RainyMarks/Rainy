/** Human runtime discovery and explicit environment selection; no package installation is implicit. */
import type { WorkspaceId } from './ide-files-protocol.ts'
import type { Branded } from './brand.ts'

/** An interpreter identified within one verified execution target. */
export type RuntimeEnvironmentId = Branded<'RainyRuntimeEnvironmentId'>
/** A native Windows installation or one registered WSL distribution. */
export type ExecutionTargetId = Branded<'RainyExecutionTargetId'>

/** Interpreter families whose executables are resolved per workspace. */
export type RuntimeLanguage = 'python' | 'node' | 'php' | 'c' | 'cpp'
/** Host execution world reported by a verified interpreter probe. */
export type RuntimePlatform = 'windows' | 'linux'
/** A bounded functional probe; missing capabilities remain visible individually. */
export interface RuntimeCapability { name: string; ready: boolean; detail?: string | undefined }
/** An existing or bundled executable observed in the current Host. */
export interface RuntimeCandidate {
  id: RuntimeEnvironmentId
  language: RuntimeLanguage
  path: string
  source: 'system' | 'project' | 'conda' | 'bundled' | 'manual'
  platform: RuntimePlatform
  version: string | null
  prefix?: string | undefined
  ready: boolean
  capabilities: RuntimeCapability[]
  error?: string | undefined
}
/** Current workspace defaults and the latest discovery result. */
export interface RuntimeSnapshot {
  targetId: ExecutionTargetId
  platform: RuntimePlatform
  workspaceId: WorkspaceId
  selected: Partial<Record<RuntimeLanguage, RuntimeCandidate>>
  candidates: RuntimeCandidate[]
}
/** Authenticated human operations accepted by the runtime page. */
export type RuntimeRequest =
  | { op: 'status' | 'discover'; workspaceId: WorkspaceId }
  | { op: 'probe'; workspaceId: WorkspaceId; language: RuntimeLanguage; path: string }
  | { op: 'select'; workspaceId: WorkspaceId; language: RuntimeLanguage; path: string | null }
