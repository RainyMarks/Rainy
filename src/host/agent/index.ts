/** Agent side of the Host: chats, models, MCP, skills and project memory, served over RPC. */
import { readdir, rm, stat } from 'node:fs/promises'
import { join } from 'node:path'
import { toToolDeclaration, type Tool } from '@earendil-works/pi-ai'
import { z } from 'zod'
import { resolveBudget } from '../../shared/budget.ts'
import { brandString } from '../../shared/brand.ts'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { ProjectRegistry } from '../../shared/project-registry.ts'
import type { BudgetPreview, ExtensionsStatus, McpServerConfig, SecurityStatus, SessionSummary } from '../../shared/rpc.ts'
import type { Activity } from '../activity.ts'
import type { HostEnvironment } from '../env.ts'
import type { Projects } from '../projects.ts'
import { RpcError, type RpcHub } from '../rpc.ts'
import type { RuntimeService } from '../runtime/index.ts'
import type { Settings } from '../settings.ts'
import { estimateMessages } from './compaction.ts'
import { completeChats, completeFiles } from './complete.ts'
import { loadBaseline, renderBaseline } from './instructions.ts'
import { idaServer, McpManager } from './mcp.ts'
import { ProjectMemory } from './memory/index.ts'
import { DEEPSEEK_FLASH, Models } from './models.ts'
import { buildSystemPrompt } from './prompt.ts'
import { ChatSession } from './session.ts'
import type { AgentServices } from './services.ts'
import { discoverSkills, skillsSection } from './skills.ts'
import { ChangeSnapshots } from './snapshots.ts'
import { ChatStore } from './store.ts'
import { inertCoreTools } from './tools/index.ts'

/** Spill files and undo snapshots older than this are deleted at startup. */
const SPILL_RETENTION_MS = 30 * 24 * 3600 * 1000

const permissionSchema = z.enum(['read-only', 'ask', 'auto'])
const approveSchema = z.object({
  sessionId: z.string().min(1), toolCallId: z.string().min(1).max(512), decision: z.enum(['allow', 'always', 'deny']), reason: z.string().max(4000).optional(),
})
const revertSchema = z.object({ sessionId: z.string().min(1), toolCallIds: z.array(z.string().min(1).max(512)).min(1).max(1000) })
const removeRuleSchema = z.object({ workspaceId: z.string().min(1), kind: z.enum(['command', 'tool']), rule: z.string().min(1).max(200) })

/**
 * The ripgrep the grep and glob tools run: the installation's own copy, else the npm package's.
 * @param appRoot Application root, whose `bin` holds ripgrep in packaged builds.
 * @returns The executable, or `undefined` when there is none.
 */
async function findRipgrep(appRoot: string): Promise<string | undefined> {
  const bundled = join(appRoot, 'bin', process.platform === 'win32' ? 'rg.exe' : 'rg')
  if (await stat(bundled).then(info => info.isFile(), () => false)) return bundled
  try { return (await import('@vscode/ripgrep')).rgPath } catch (_missingPlatformPackage) { return undefined }
}

/** What `createAgent` needs. */
export interface AgentDeps {
  env: HostEnvironment
  settings: Settings
  projects: Projects
  registry: ProjectRegistry
  runtime: RuntimeService
  rpc: RpcHub
  activity: Activity
  log(message: string): void
}

/** Agent operations the Host entry uses directly. */
export interface AgentService {
  /** `GET /rainy/control` answer for the desktop carrier. */
  controlStatus(): { models: unknown[]; selected: unknown }
  /** `connect-strata` from the desktop carrier. */
  connectStrata(params: unknown): Promise<{ provider: string; model: string }>
  close(): Promise<void>
}

/**
 * Build the agent side and register its RPC methods.
 * @param deps Host services.
 * @returns Operations used by the Host entry.
 */
export async function createAgent(deps: AgentDeps): Promise<AgentService> {
  const { env, settings, projects, rpc, log } = deps
  const store = new ChatStore(join(env.home, 'chats'))
  await store.load()
  const models = new Models(settings)
  if (env.configureDeepSeek === true) await models.configure(DEEPSEEK_FLASH)
  const mcp = new McpManager(log)
  const sessions = new Map<string, ChatSession>()
  const opening = new Map<string, Promise<ChatSession>>()
  let activeRequests = 0
  let lastModelActivity = 0
  let lastExtensionsCwd: string | undefined

  const memory = new ProjectMemory({
    root: join(env.carrierStateRoot, 'project-memory'),
    target: env.executionTargetId,
    models,
    projectOf: async (workspaceId) => {
      const project = projects.get(workspaceId)
      if (project === undefined) throw new RpcError('workspace-not-found', '工作区不存在。')
      return deps.registry.getOrRegister({ workspaceId: project.id, path: project.path, title: project.title })
    },
    readChat: async (sessionId) => {
      const file = await store.read(sessionId)
      return { workspaceId: file.chat.workspaceId, cwd: file.chat.cwd, entries: file.entries }
    },
    modelFor: sessionId => models.resolve(store.get(sessionId)?.model),
    busy: () => deps.activity.active() || activeRequests > 0,
    lastModelActivity: () => lastModelActivity,
    changed: (workspaceId) => { rpc.emit('memory.changed', { workspaceId }) },
    log,
  })

  const snapshots = new ChangeSnapshots(join(env.home, 'snapshots'))
  const services: AgentServices = {
    env, settings, models, store, mcp, memory, projects, runtime: deps.runtime, activity: deps.activity, rpc, log, snapshots, ripgrep: await findRipgrep(env.appRoot),
    modelBusy: () => { activeRequests = Math.max(1, activeRequests); lastModelActivity = Date.now() },
    modelIdle: () => { activeRequests = [...sessions.values()].filter(session => session.running).length; lastModelActivity = Date.now() },
  }
  deps.activity.addSource(() => [...sessions.values()].some(session => session.running))

  mcp.sync(settings.get().mcpServers)
  mcp.onChange(() => { void extensionsStatus(lastExtensionsCwd).then(status => { rpc.emit('extensions.changed', status) }) })
  models.onChange(status => { rpc.emit('models.changed', status) })
  void sweepSpill(join(env.home, 'spill')).catch((error: unknown) => { log(`[spill] sweep failed: ${String(error)}`) })
  void snapshots.sweep(SPILL_RETENTION_MS).catch((error: unknown) => { log(`[snapshots] sweep failed: ${String(error)}`) })

  const session = (id: string): Promise<ChatSession> => {
    const loaded = sessions.get(id)
    if (loaded !== undefined) return Promise.resolve(loaded)
    let pending = opening.get(id)
    if (pending === undefined) {
      if (store.get(id) === undefined) return Promise.reject(new RpcError('not-found', '对话不存在。'))
      pending = ChatSession.open(services, id).then((opened) => { sessions.set(id, opened); return opened }).finally(() => { opening.delete(id) })
      opening.set(id, pending)
    }
    return pending
  }
  const summary = (id: string): SessionSummary => {
    const live = sessions.get(id)
    if (live !== undefined) return live.summary()
    const chat = store.get(id)
    if (chat === undefined) throw new RpcError('not-found', '对话不存在。')
    return { ...chat, status: 'idle' }
  }
  const securityStatus = (workspaceId: WorkspaceId | null | undefined): SecurityStatus => ({
    defaultMode: settings.defaultPermission(),
    rules: workspaceId === null || workspaceId === undefined ? { commands: [], tools: [] } : settings.permissionRules(workspaceId),
  })
  const cwdFor = (workspaceId: WorkspaceId | null | undefined): string | undefined =>
    workspaceId === null || workspaceId === undefined ? undefined : projects.get(workspaceId)?.path

  async function extensionsStatus(cwd: string | undefined): Promise<ExtensionsStatus> {
    return {
      skills: await discoverSkills(cwd),
      servers: settings.get().mcpServers,
      status: mcp.status(),
      idaAvailable: env.idaMcpCommand !== undefined,
    }
  }
  async function saveServers(change: (servers: McpServerConfig[]) => McpServerConfig[]): Promise<ExtensionsStatus> {
    const data = await settings.update((current) => { current.mcpServers = change(current.mcpServers) })
    mcp.sync(data.mcpServers)
    const status = await extensionsStatus(lastExtensionsCwd)
    rpc.emit('extensions.changed', status)
    return status
  }

  // ── models ──
  rpc.register('models.status', () => models.status())
  rpc.register('models.configure', input => models.configure(input))
  rpc.register('models.remove', ({ provider }) => models.remove(provider))
  rpc.register('models.select', selection => models.select(selection))
  rpc.register('models.discover', input => models.discover(input))
  rpc.register('models.probe', input => models.probe(input))
  rpc.register('prompt.global', ({ text }) => models.setGlobalPrompt(text))
  rpc.register('budget.preview', async ({ workspaceId, sessionId, draft }) => {
    const cwd = cwdFor(workspaceId)
    if (cwd === undefined) throw new RpcError('workspace-not-found', '工作区不存在。')
    const chat = sessionId === undefined ? undefined : await session(sessionId)
    const resolved = chat?.resolved() ?? models.resolve()
    if (resolved === undefined) throw new RpcError('no-model', '请先配置模型。')
    const budget = resolveBudget(resolved.setup.contextWindow, resolved.setup.maxTokens)
    const skills = skillsSection(await discoverSkills(cwd))
    const core = inertCoreTools(env.platform)
    const system = buildSystemPrompt({ cwd, platform: env.platform, tools: core.map(tool => tool.name), mcpInstructions: mcp.instructions(), extraRoots: [], skills: '', globalPrompt: '' })
    const coreDeclarations: Tool[] = core.map(tool => toToolDeclaration(tool))
    const mcpDeclarations: Tool[] = mcp.tools().map(tool => toToolDeclaration(tool))
    const instructions = renderBaseline(await loadBaseline(env.home, cwd)) ?? ''
    const recall = await memory.recall(workspaceId, budget.inputLimit).catch(() => undefined) ?? ''
    const history = chat === undefined ? [] : chat.snapshot().entries.flatMap(entry => entry.kind === 'user' ? [{ role: 'user' as const, content: entry.text, timestamp: entry.ts }] : [])
    const empty = estimateMessages('', [], [])
    const systemTokens = estimateMessages(system, [], []) - empty
    const extensions = estimateMessages(skills, mcpDeclarations, []) - empty
    const text = (value: string): number => value === '' ? 0 : estimateMessages('', [], [{ role: 'user', content: value, timestamp: 0 }])
    const historyTokens = history.length === 0 ? 0 : estimateMessages('', [], history)
    const preview: BudgetPreview = {
      model: resolved.setup.model, contextWindow: budget.contextWindow, inputLimit: budget.inputLimit, outputTokens: budget.outputTokens,
      marginTokens: budget.marginTokens, kind: 'estimated', tokens: 0,
      breakdown: {
        system: systemTokens, tools: estimateMessages('', coreDeclarations, []) - empty, extensions, instructions: text(instructions) + text(settings.get().globalPrompt),
        memory: text(recall), history: historyTokens + text(draft ?? ''), framing: empty,
      },
    }
    preview.tokens = Object.values(preview.breakdown).reduce((total, value) => total + value, 0)
    return preview
  })

  // ── chats ──
  rpc.register('sessions.list', ({ workspaceId, archived }) => store.list()
    .filter(chat => workspaceId === undefined || chat.workspaceId === workspaceId)
    .filter(chat => archived === undefined || chat.archived === archived)
    .map(chat => summary(chat.id)))
  rpc.register('sessions.search', async ({ query, limit }) => (await store.search(query, Math.min(limit ?? 20, 50))).map(hit => ({ summary: summary(hit.chat.id), snippet: hit.snippet })))
  rpc.register('sessions.create', async ({ workspaceId, cwd, permission }) => {
    const directory = cwdFor(workspaceId) ?? cwd ?? env.home
    const mode = permission === undefined ? undefined : permissionSchema.parse(permission)
    const chat = await store.create({ workspaceId, cwd: directory, ...(mode === undefined ? {} : { permission: mode }) })
    const created = summary(chat.id)
    rpc.emit('sessions.changed', created)
    return created
  })
  rpc.register('sessions.get', async ({ sessionId, limit }) => (await session(sessionId)).snapshot(limit))
  rpc.register('sessions.rename', async ({ sessionId, title }) => {
    const trimmed = title.trim()
    if (trimmed === '' || Buffer.byteLength(trimmed, 'utf8') > 80) throw new RpcError('invalid', '标题需为 1–80 字节。')
    await store.setMeta(sessionId, { title: trimmed })
    const changed = summary(sessionId)
    rpc.emit('sessions.changed', changed)
    return changed
  })
  rpc.register('sessions.archive', async ({ sessionId, archived }) => {
    if (archived) sessions.get(sessionId)?.abort()
    await store.setMeta(sessionId, { archived })
    const changed = summary(sessionId)
    rpc.emit('sessions.changed', changed)
    return changed
  })
  rpc.register('sessions.pin', async ({ sessionId, pinned }) => {
    await store.setMeta(sessionId, { pinned })
    const changed = summary(sessionId)
    rpc.emit('sessions.changed', changed)
    return changed
  })
  rpc.register('sessions.delete', async ({ sessionId }) => {
    const live = sessions.get(sessionId)
    if (live !== undefined) { await live.close(); sessions.delete(sessionId) }
    await store.delete(sessionId)
    await snapshots.remove(sessionId).catch((error: unknown) => { log(`[snapshots] ${sessionId}: ${String(error)}`) })
    rpc.emit('sessions.removed', { sessionId })
  })
  rpc.register('sessions.fork', async ({ sessionId, entryId }) => {
    const file = await store.read(sessionId)
    const index = file.entries.findIndex(entry => entry.id === entryId)
    if (index < 0) throw new RpcError('not-found', '消息不存在。')
    let end = index
    // Keep the tool results that answer calls in the cut-off reply.
    while (end + 1 < file.entries.length && file.entries[end + 1]!.kind === 'toolResult') end++
    const chat = await store.create({
      workspaceId: file.chat.workspaceId, cwd: file.chat.cwd, parent: { sessionId, entryId }, entries: file.entries.slice(0, end + 1),
      title: `${file.chat.title || '新对话'}（分支）`, model: file.chat.model, ...(file.chat.permission === undefined ? {} : { permission: file.chat.permission }),
    })
    const created = summary(chat.id)
    rpc.emit('sessions.changed', created)
    return created
  })

  rpc.register('chat.send', async ({ sessionId, text, images, mode }) => (await session(sessionId)).send(text, images ?? [], mode ?? 'queue'))
  rpc.register('chat.abort', async ({ sessionId }) => { sessions.get(sessionId)?.abort() })
  rpc.register('chat.unqueue', async ({ sessionId, id }) => (await session(sessionId)).unqueue(id))
  rpc.register('chat.setModel', async ({ sessionId, ...selection }) => (await session(sessionId)).setModel(selection))
  rpc.register('chat.compact', async ({ sessionId }) => ({ message: await (await session(sessionId)).compactNow() }))
  rpc.register('chat.setPermission', async ({ sessionId, mode }) => (await session(sessionId)).setPermission(permissionSchema.parse(mode)))
  rpc.register('chat.approve', async (params) => {
    const { sessionId, toolCallId, decision, reason } = approveSchema.parse(params)
    const live = sessions.get(sessionId)
    if (live === undefined) throw new RpcError('not-found', '这个操作已不在等待批准。')
    await live.approve(toolCallId, decision, reason)
  })
  rpc.register('chat.revert', async (params) => {
    const { sessionId, toolCallIds } = revertSchema.parse(params)
    return (await session(sessionId)).revert(toolCallIds)
  })
  rpc.register('chat.complete', async ({ sessionId, workspaceId, query }) => {
    const chat = sessionId === undefined ? undefined : store.get(sessionId)
    const cwd = chat?.cwd ?? cwdFor(workspaceId)
    const files = cwd === undefined ? [] : await completeFiles(cwd, query)
    return [...files, ...completeChats(store.list(), query, sessionId)]
  })

  // ── security ──
  rpc.register('security.status', ({ workspaceId }) => securityStatus(workspaceId))
  rpc.register('security.setDefault', async ({ mode }) => {
    await settings.setDefaultPermission(permissionSchema.parse(mode))
    for (const live of sessions.values()) live.refresh()
    return securityStatus(undefined)
  })
  rpc.register('security.removeRule', async (params) => {
    const { workspaceId, kind, rule } = removeRuleSchema.parse(params)
    await settings.setPermissionRule(workspaceId, kind, rule, false)
    return securityStatus(brandString<WorkspaceId>(workspaceId))
  })

  // ── skills and MCP ──
  rpc.register('extensions.status', async ({ cwd }) => { lastExtensionsCwd = cwd; return extensionsStatus(cwd) })
  rpc.register('extensions.saveServer', ({ server, previousName }) => saveServers((servers) => {
    if (server.name !== previousName && servers.some(item => item.name === server.name)) throw new RpcError('duplicate', `已存在名为 ${server.name} 的 MCP 服务器。`)
    const index = servers.findIndex(item => item.name === (previousName ?? server.name))
    return index < 0 ? [...servers, server] : servers.map((item, position) => position === index ? server : item)
  }))
  rpc.register('extensions.removeServer', ({ name }) => saveServers(servers => servers.filter(item => item.name !== name)))
  rpc.register('extensions.addIda', () => {
    if (env.idaMcpCommand === undefined) throw new RpcError('unavailable', '未找到 uvx，无法启动 IDA MCP。')
    return saveServers(servers => [...servers.filter(item => item.name !== 'ida'), idaServer(env.idaMcpCommand!)])
  })

  // ── project memory ──
  rpc.register('memory.status', ({ workspaceId }) => memory.status(workspaceId))
  rpc.register('memory.setEnabled', ({ workspaceId, ...values }) => memory.setEnabled(workspaceId, values))
  rpc.register('memory.edit', ({ workspaceId, ...edit }) => memory.edit(workspaceId, edit))
  rpc.register('memory.delete', ({ workspaceId, id }) => memory.remove(workspaceId, id))
  rpc.register('memory.clear', ({ workspaceId }) => memory.remove(workspaceId))

  return {
    controlStatus: () => ({ models: models.status().models, selected: models.status().selected }),
    connectStrata: params => models.connectStrata(params),
    async close() {
      await Promise.allSettled([...sessions.values()].map(chat => chat.close()))
      await memory.close()
      await mcp.close()
      await store.flush()
    },
  }
}

async function sweepSpill(root: string): Promise<void> {
  let sessions: string[]
  try { sessions = await readdir(root) } catch (_error) { return }
  const cutoff = Date.now() - SPILL_RETENTION_MS
  for (const name of sessions) {
    const directory = join(root, name)
    for (const file of await readdir(directory).catch(() => [] as string[])) {
      const path = join(directory, file)
      const info = await stat(path).catch(() => undefined)
      if (info !== undefined && info.mtimeMs < cutoff) await rm(path, { force: true })
    }
  }
}
