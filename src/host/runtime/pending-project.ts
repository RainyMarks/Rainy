/** Restore the carrier-selected project before the target Host announces readiness. */
import type { Context } from '@deepseek-ai/cordis'
import type { IdeWorkspaceRoot } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'
import type { ProjectId, ExecutionTargetId } from './project-registry.ts'
import type {} from './runtime.ts'
import type {} from './project-roots.ts'
import type {} from './ide-state.ts'

/** Validated carrier paths and identities in the target Host's execution environment. */
export interface PendingHostProject {
  projectId: ProjectId
  targetId: ExecutionTargetId
  path: string
  roots: readonly Pick<IdeWorkspaceRoot, 'rootId' | 'path' | 'title'>[]
}

/** Bind, restore mounts and select the migrated project without changing either Host's recovery rows or Sessions.
 * @param ctx Initialized workspace, project and IDE storage owners.
 * @param pending Carrier selection with paths mapped into this Host.
 * @returns Completion after the public target and selected workspace are durable.
 */
export async function restorePendingHostProject(ctx: Context, pending: PendingHostProject): Promise<void> {
  const workspace = await ctx.workspaceRegistry.create(pending.path)
  const projectId = await ctx.rainyRuntime.projects.getOrRegister({ workspaceId: workspace.id, path: workspace.path,
    title: workspace.title, projectId: pending.projectId })
  for (const root of pending.roots) await ctx.rainyProjectRoots.importRoot(workspace.id, root)
  await ctx.rainyRuntime.projects.selectTarget(projectId, pending.targetId)
  await ctx.rainyIdeState.setSelection(workspace.id)
}
