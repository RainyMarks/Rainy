/** Private native Host using the same named DSH profile on Windows and WSL. */
import { mkdirSync, existsSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import { delimiter, dirname, join, resolve } from 'node:path'
import { createInterface } from 'node:readline'
import { loadLayeredEnv, initProfile, loadProfileDirectory, reportSkippedBundles } from '@deepseek-ai/dsh-app-boot'
import { runProfile } from '../../cli/src/profile-boot.ts'
import type {} from '@deepseek-ai/dsh-host-webserver'
import type {} from '@deepseek-ai/dsh-client-connection'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { IdeRootId } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'
import { z } from 'zod'
import { configureModel, DEEPSEEK_FLASH, migrateSavedModelThinking } from './models.ts'
import type {} from './runtime.ts'
import { ExecutionTargetId, ProjectId } from './project-registry.ts'
import type {} from './project-roots.ts'
import { restorePendingHostProject } from './host-project.ts'

const appDir = resolve(import.meta.dirname, '..')
const home = process.env.RAINY_HOME ?? join(homedir(), '.rainy-agent')
process.env.DSH_HOME = home
process.env.DSH_TELEMETRY_DISABLED = '1'
process.env.PATH = [dirname(process.execPath), resolve(appDir, '../bin'), process.env.PATH].filter(Boolean).join(delimiter)
const profileDir = join(home, 'profiles', 'rainy')
mkdirSync(profileDir, { recursive: true, mode: 0o700 })
if (!existsSync(join(profileDir, 'package.json'))) initProfile(profileDir, ['@deepseek-ai/dsh-rainy-desktop'])
const send = (message: object) => process.stdout.write(`RAINY_CONTROL ${JSON.stringify(message)}\n`)
try {
  const profile = loadProfileDirectory('dsh', profileDir, join(appDir, 'package.json'))
  reportSkippedBundles('RainyAgent', profile)
  if (profile.skippedBundles.length) throw new Error('Rainy runtime bundle could not be loaded.')
  const { ctx, shutdown } = await runProfile({
    environment: loadLayeredEnv('dsh'), profile: 'rainy', args: [], patchFiles: [],
    resolvedProfile: { profile, installAnchor: join(appDir, 'package.json') },
  })
  await ctx.loader.await()
  ctx.logger.exporter({ levels: { default: 2 }, export: (record) => {
    if (record.type === 'warn' || record.type === 'error') console.error(`[${record.name}]`, record.args as unknown[])
  } })
  if (!ctx.get('connection')) {
    console.error('Rainy boot entries:', [...ctx.loader.entries()].map(entry => ({ id: entry.options.id, state: entry.fiber?.state })))
    throw new Error('Rainy HTTP connection service did not start.')
  }
  try {
    await migrateSavedModelThinking(ctx)
  } catch (error) {
    await shutdown.shutdown(1)
    throw error
  }
  if (process.env.RAINY_CONFIGURE_DEEPSEEK === '1') await configureModel(ctx, DEEPSEEK_FLASH)
  if (process.env.RAINY_PENDING_PROJECT_ID && process.env.RAINY_PENDING_PROJECT_PATH) {
    const roots = process.env.RAINY_PENDING_PROJECT_ROOTS
      ? z.array(z.object({
        rootId: z.string().min(1).transform(value => brandString<IdeRootId>(value)),
        path: z.string().min(1), title: z.string(),
      }).strict())
        .parse(JSON.parse(process.env.RAINY_PENDING_PROJECT_ROOTS))
      : []
    await restorePendingHostProject(ctx, {
      projectId: ProjectId(process.env.RAINY_PENDING_PROJECT_ID), path: process.env.RAINY_PENDING_PROJECT_PATH, roots,
      targetId: ExecutionTargetId(process.env.RAINY_EXECUTION_TARGET_ID ?? (process.platform === 'win32' ? 'windows-local' : 'wsl:legacy')),
    })
  }
  if (ctx.get('rainyRuntime')) for (const workspace of ctx.workspaceRegistry.list()) {
    await ctx.rainyRuntime.projects.getOrRegister({ workspaceId: workspace.id, path: workspace.path, title: workspace.title })
  }
  const ready = { type: 'ready', protocol: 1, url: ctx.connection.authenticatedUrl(`http://127.0.0.1:${ctx.webServer.port}`), pid: process.pid, home }
  if (process.env.RAINY_READY_FILE) writeFileSync(process.env.RAINY_READY_FILE, JSON.stringify(ready), { mode: 0o600 })
  send(ready)
  const input = createInterface({ input: process.stdin, crlfDelay: Infinity })
  let stopping = false
  const stop = async () => {
    if (stopping) return
    stopping = true; input.close()
    await shutdown.shutdown(0)
    process.exitCode = 0
  }
  input.on('line', (line) => {
    try {
      const command: unknown = JSON.parse(line)
      if (command !== null && typeof command === 'object' && 'type' in command) {
        if (command.type === 'stop') void stop()
        else if (command.type === 'inspect-activity' && 'id' in command && typeof command.id === 'string') {
          const active = ctx.agents.list().some(agent => agent.status === 'running' || agent.inbox.nextTurn.length > 0 || agent.inbox.nextStep.length > 0)
            || ctx.get('rainyRuntime')?.hasActivity() === true
          if ('mode' in command && command.mode === 'resume') ctx.rainyRuntime.setSwitchPending(false)
          else if ('mode' in command && command.mode === 'freeze' && !active) ctx.rainyRuntime.setSwitchPending(true)
          send({ type: 'activity', id: command.id, active })
        }
        else if (command.type === 'inspect-project' && 'id' in command && typeof command.id === 'string'
          && (!('workspaceId' in command) || typeof command.workspaceId === 'string')) {
          const id = command.id
          const selected = 'workspaceId' in command && typeof command.workspaceId === 'string' ? WorkspaceId(command.workspaceId)
            : ctx.rainyIdeState.getSelection().workspaceId
          const workspace = selected === null ? undefined : ctx.workspaceRegistry.get(selected)
          if (selected === null) send({ type: 'project', id, project: null })
          else if (!workspace) send({ type: 'project', id, error: 'workspace-unavailable' })
          else void ctx.rainyRuntime.projects.getOrRegister({
            workspaceId: workspace.id, path: workspace.path, title: workspace.title,
          }).then((projectId) => {
            send({ type: 'project', id, project: { projectId, workspaceId: workspace.id, roots: ctx.rainyProjectRoots.get(workspace.id) } })
          }).catch(() => { send({ type: 'project', id, error: 'project-unavailable' }) })
        }
      }
    } catch (error) { console.error(error instanceof Error ? error.message : 'Invalid control message') }
  })
  if (process.env.RAINY_DETACHED !== '1') input.on('close', () => { void stop() })
} catch (error) {
  send({ type: 'fatal', message: error instanceof Error ? error.message : 'Host startup failed' })
  process.exitCode = 1
}
