/** Workspace runtime selection and authenticated human discovery over native Host providers. */
import { Context, Service } from '@deepseek-ai/cordis'
import { homedir } from 'node:os'
import { join } from 'node:path'
import z from '@deepseek-ai/schemastery'
import { z as json } from 'zod'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type {} from '@deepseek-ai/dsh-host-webserver'
import type {} from '@deepseek-ai/dsh-client-connection'
import type {} from '@deepseek-ai/dsh-subprocess'
import { RuntimeEnvironments } from './runtime-environments.ts'
import type { ResolvedWorkspaceEnvironment } from './runtime-environments.ts'
import { createProjectRegistry } from './project-registry.ts'
import type { ProjectRegistry } from './project-registry.ts'
import { readRequestBytes } from './request-body.ts'
import type {} from '@deepseek-ai/dsh-agent'

/** Runtime plugin identity. */
export const name = 'rainy-runtime'
/** Native execution, workspace authority and authenticated application routes. */
export const inject = ['subprocess', 'workspaceRegistry', 'connection', 'webServer']
/** Bounded interpreter discovery configuration. */
export interface Config {
  probeTimeoutMs: number
  maxCandidates: number
  /** Interpreter probes run at the same time during discovery. */
  probeConcurrency: number
  maxRequestBytes: number
  /** Explicit owned state location for isolated profiles; absence uses the launched Harness home. */
  dataRoot?: string
  /** Carrier project catalog directory; tests supply their own private directory. */
  carrierStateRoot?: string
  /** Native execution target identity chosen by the carrier. */
  executionTargetId?: string
  /** Optional component installation root in this execution world. */
  bundledRoot?: string
  /** PHP shipped with the carrier for this execution world; installed components and project selections take precedence. */
  builtinPhp?: string
}
/** Deployment limits; the request cannot override them. */
export const Config: z<Config> = z.object({
  probeTimeoutMs: z.number().min(1000).max(120000).default(30000),
  maxCandidates: z.number().min(1).max(64).default(16),
  probeConcurrency: z.number().min(1).max(16).step(1).default(4),
  maxRequestBytes: z
    .number()
    .min(1024)
    .max(1024 * 1024)
    .default(65536),
  dataRoot: z.string().required(false),
  carrierStateRoot: z.string().required(false),
  executionTargetId: z.string().required(false),
  bundledRoot: z.string().required(false),
  builtinPhp: z.string().required(false),
})

declare module '@deepseek-ai/cordis' {
  interface Context {
    rainyRuntime: RainyRuntime
  }
}

/** Interpreter resolver shared by all product execution consumers. */
export class RainyRuntime extends Service {
  private readonly activitySources = new Set<() => boolean>()
  private switching = false
  private activeOperations = 0
  /** @param ctx - native Host context. @param environments - initialized selection owner. @param projects - carrier catalog singleton. */
  constructor(
    ctx: Context,
    readonly environments: RuntimeEnvironments,
    readonly projects: ProjectRegistry,
  ) {
    super(ctx, 'rainyRuntime')
  }
  /** @param workspaceId - current Host workspace. @returns the workspace's explicit process environment. */
  resolveWorkspace(workspaceId: WorkspaceId): ResolvedWorkspaceEnvironment {
    this.assertCanStart()
    const workspace = this.ctx.workspaceRegistry.get(workspaceId)
    if (!workspace) throw new Error('The selected workspace is unavailable.')
    return this.environments.resolve(workspace.path)
  }
  /** @param cwd - the actual process directory. @returns its nearest selected workspace environment. */
  resolveDirectory(cwd: string): ResolvedWorkspaceEnvironment {
    this.assertCanStart()
    return this.environments.resolve(cwd)
  }
  /** Reject newly admitted execution while the idle Host is being replaced. */
  assertCanStart(): void {
    if (this.switching) throw new Error('执行环境正在切换，请等待应用重新打开。')
  }
  /** Reserve one interpreter-discovery operation while it owns processes.
   * @returns an idempotent release callback after the operation settles.
   */
  beginOperation(): () => void {
    this.assertCanStart()
    this.activeOperations++
    let released = false
    return () => {
      if (!released) {
        released = true
        this.activeOperations--
      }
    }
  }
  /** @param pending - carrier-owned freeze after synchronous idle inspection. */
  setSwitchPending(pending: boolean): void {
    this.switching = pending
  }
  /** @param inspect - one owned process controller. @returns its unregister callback. */
  registerActivitySource(inspect: () => boolean): () => void {
    this.activitySources.add(inspect)
    return () => {
      this.activitySources.delete(inspect)
    }
  }
  /** @returns whether a registered human execution controller still owns work. */
  hasActivity(): boolean {
    return this.activeOperations > 0 || [...this.activitySources].some(inspect => inspect())
  }
}

const workspaceId = json.string().min(1).transform(WorkspaceId)
const language = json.enum(['python', 'node', 'php', 'c', 'cpp'])
const requestSchema = json.discriminatedUnion('op', [
  json.object({ op: json.enum(['status', 'discover']), workspaceId }).strict(),
  json.object({ op: json.literal('probe'), workspaceId, language, path: json.string().min(1).max(32768) }).strict(),
  json
    .object({ op: json.literal('select'), workspaceId, language, path: json.string().min(1).max(32768).nullable() })
    .strict(),
])

/** @param ctx - composed native Host. @param config - validated limits. @returns completion after state and routes are ready. */
export async function apply(ctx: Context, config: Config): Promise<void> {
  const home = config.dataRoot ?? process.env.RAINY_HOME ?? process.env.DSH_HOME ?? join(homedir(), '.rainy-agent')
  const targetId =
    config.executionTargetId ??
    process.env.RAINY_EXECUTION_TARGET_ID ??
    (process.platform === 'win32' ? 'windows-local' : 'wsl:legacy')
  const builtinPhp = config.builtinPhp ?? process.env.RAINY_BUILTIN_PHP
  const environments = new RuntimeEnvironments({
    root: home,
    targetId,
    bundledRoot: config.bundledRoot ?? process.env.RAINY_TOOLCHAIN_ROOT ?? join(home, 'components'),
    builtinExecutables: { ...builtinPhp === undefined ? {} : { php: builtinPhp } },
    probeTimeoutMs: config.probeTimeoutMs,
    maxCandidates: config.maxCandidates,
    probeConcurrency: config.probeConcurrency,
    resolveWorkspace: id => ctx.workspaceRegistry.get(id),
  })
  await environments.initialize()
  const projects = createProjectRegistry({
    root: config.carrierStateRoot ?? process.env.RAINY_CARRIER_STATE_ROOT ?? join(home, 'carrier-state'),
    targetId,
  })
  for (const workspace of ctx.workspaceRegistry.list())
    await projects.getOrRegister({ workspaceId: workspace.id, path: workspace.path, title: workspace.title })
  new RainyRuntime(ctx, environments, projects)
  ctx.on(
    'agent/request',
    async (_request, next) => {
      ctx.rainyRuntime.assertCanStart()
      return next()
    },
    { prepend: true },
  )
  let closing = false
  ctx.effect(() => () => {
    closing = true
  })
  ctx.effect(() =>
    ctx.webServer.register({
      kind: 'exact',
      path: '/rainy/runtime',
      handler: async (request, response) => {
        const admission = ctx.connection.admit(request)
        if ('rejection' in admission) {
          response.writeHead(admission.rejection)
          response.end()
          return
        }
        response.setHeader('Content-Type', 'application/json; charset=utf-8')
        response.setHeader('Cache-Control', 'no-store')
        if (request.method !== 'POST') {
          response.writeHead(405)
          response.end()
          return
        }
        if (closing) {
          response.writeHead(503)
          response.end(JSON.stringify({ ok: false, error: 'The runtime service is closing.' }))
          return
        }
        try {
          const body = await readRequestBytes(request, config.maxRequestBytes,
            () => new Error('Runtime request exceeds its size limit.'))
          const command = requestSchema.parse(JSON.parse(body.toString('utf8')))
          const release = command.op === 'status' ? undefined : ctx.rainyRuntime.beginOperation()
          try {
            const workspace = ctx.workspaceRegistry.get(command.workspaceId)
            if (!workspace) throw new Error('The selected workspace is unavailable.')
            await projects.getOrRegister({ workspaceId: workspace.id, path: workspace.path, title: workspace.title })
            const result =
              command.op === 'discover'
                ? await environments.discover(command.workspaceId)
                : command.op === 'probe'
                  ? await environments.probe(command.workspaceId, command.language, command.path)
                  : command.op === 'select'
                    ? await environments.select(command.workspaceId, command.language, command.path)
                    : await environments.status(command.workspaceId)
            response.end(JSON.stringify({ ok: true, result }))
          } finally {
            release?.()
          }
        } catch (error) {
          response.statusCode = 400
          response.end(
            JSON.stringify({ ok: false, error: error instanceof Error ? error.message : 'Runtime inspection failed.' }),
          )
        }
      },
    }),
  )
}
