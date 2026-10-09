/** Interpreter discovery and per-project selection, served through the `runtime` RPC method. */
import { z } from 'zod'
import { brandString } from '../../shared/brand.ts'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { ProjectRegistry } from '../../shared/project-registry.ts'
import type { RuntimeRequest, RuntimeSnapshot } from '../../shared/runtime-protocol.ts'
import type { Activity } from '../activity.ts'
import type { HostEnvironment } from '../env.ts'
import type { Projects } from '../projects.ts'
import { RpcError } from '../rpc.ts'
import { RuntimeEnvironments, type ResolvedWorkspaceEnvironment } from './environments.ts'

export type { ResolvedWorkspaceEnvironment } from './environments.ts'

/** Wait for one interpreter probe. */
export const RUNTIME_PROBE_TIMEOUT_MS = 30000
/** Candidates kept per discovery. */
export const RUNTIME_MAX_CANDIDATES = 16
/** Interpreter probes run at the same time during discovery. */
export const RUNTIME_PROBE_CONCURRENCY = 4

/** Interpreter selections as used by the IDE, the agent's shell tools and the `runtime` RPC method. */
export interface RuntimeService {
  /**
   * Validate and serve one runtime request.
   * @param request Untrusted request from the renderer.
   * @returns The project's selections and latest discovery result.
   * @throws RpcError `invalid-request` for malformed input, `runtime-error` otherwise.
   */
  handle(request: RuntimeRequest): Promise<RuntimeSnapshot>
  /**
   * Process environment overlay (PATH etc.) for a directory, from the nearest project's selections.
   * @param cwd Absolute process directory.
   * @returns The overlay and the selected executables.
   * @throws Error while the carrier is replacing this Host.
   */
  resolveDirectory(cwd: string): ResolvedWorkspaceEnvironment
  /**
   * Process environment overlay for a project's primary directory.
   * @param id Registered project.
   * @returns The overlay and the selected executables.
   * @throws Error when the project is unknown or the carrier is replacing this Host.
   */
  resolveWorkspace(id: WorkspaceId): ResolvedWorkspaceEnvironment
}

/** Dependencies of {@link createRuntime}. */
export interface RuntimeDependencies {
  readonly env: HostEnvironment
  readonly projects: Projects
  readonly registry: ProjectRegistry
  readonly activity: Activity
  readonly log: (message: string) => void
}

const workspaceId = z.string().min(1).transform(value => brandString<WorkspaceId>(value))
const language = z.enum(['python', 'node', 'php', 'c', 'cpp'])
const requestSchema = z.discriminatedUnion('op', [
  z.object({ op: z.enum(['status', 'discover']), workspaceId }).strict(),
  z.object({ op: z.literal('probe'), workspaceId, language, path: z.string().min(1).max(32768) }).strict(),
  z.object({ op: z.literal('select'), workspaceId, language, path: z.string().min(1).max(32768).nullable() }).strict(),
])

/**
 * Load saved selections and the installed offline components.
 * @param deps Host environment, project catalog, carrier registry and activity gate.
 * @returns The runtime service.
 */
export async function createRuntime(deps: RuntimeDependencies): Promise<RuntimeService> {
  const { env, projects, registry, activity } = deps
  const environments = new RuntimeEnvironments({
    root: env.home,
    targetId: env.executionTargetId,
    bundledRoot: env.toolchainRoot,
    builtinExecutables: env.builtinPhp === undefined ? {} : { php: env.builtinPhp },
    probeTimeoutMs: RUNTIME_PROBE_TIMEOUT_MS,
    maxCandidates: RUNTIME_MAX_CANDIDATES,
    probeConcurrency: RUNTIME_PROBE_CONCURRENCY,
    resolveWorkspace: id => projects.get(id),
  })
  await environments.initialize()
  let operations = 0
  activity.addSource(() => operations > 0)

  async function serve(request: z.infer<typeof requestSchema>): Promise<RuntimeSnapshot> {
    const project = projects.get(request.workspaceId)
    if (project === undefined) throw new Error('The selected workspace is unavailable.')
    await registry.getOrRegister({ workspaceId: project.id, path: project.path, title: project.title })
    switch (request.op) {
      case 'status': return environments.status(request.workspaceId)
      case 'discover': return environments.discover(request.workspaceId)
      case 'probe': return environments.probe(request.workspaceId, request.language, request.path)
      case 'select': return environments.select(request.workspaceId, request.language, request.path)
    }
  }

  return {
    async handle(input) {
      const parsed = requestSchema.safeParse(input)
      if (!parsed.success) throw new RpcError('invalid-request', parsed.error.issues.map(issue => issue.message).join(' '))
      const request = parsed.data
      try {
        if (request.op === 'status') return await serve(request)
        activity.assertCanStart()
        operations++
        try { return await serve(request) } finally { operations-- }
      } catch (error) {
        throw new RpcError('runtime-error', error instanceof Error ? error.message : 'Runtime inspection failed.')
      }
    },
    resolveDirectory(cwd) {
      activity.assertCanStart()
      return environments.resolve(cwd)
    },
    resolveWorkspace(id) {
      activity.assertCanStart()
      const project = projects.get(id)
      if (project === undefined) throw new Error('The selected workspace is unavailable.')
      return environments.resolve(project.path)
    },
  }
}
