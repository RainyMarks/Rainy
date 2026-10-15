/** Agent side of the Host: chats, models, MCP, skills and project memory, served over RPC. */
import { readdir, rm, stat } from 'node:fs/promises'
import { join } from 'node:path'
import { toToolDeclaration, type Tool } from '@earendil-works/pi-ai'
import { z } from 'zod'
import { resolveBudget } from '../../shared/budget.ts'
import { brandString } from '../../shared/brand.ts'
import type { IdeRootId, WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { localized } from '../../shared/locale.ts'
import type { ProjectRegistry } from '../../shared/project-registry.ts'
import type { BudgetPreview, ExtensionsStatus, McpServerConfig, ProxySettings, SecurityStatus, SessionSummary } from '../../shared/rpc.ts'
import type { Activity } from '../activity.ts'
import type { HostEnvironment } from '../env.ts'
import { GitService } from '../git/index.ts'
import { ProjectMaps } from '../map/index.ts'
import { PASSWORD_MASK, proxyUrl, type Network } from '../network.ts'
import type { Projects } from '../projects.ts'
import { RpcError, type RpcHub } from '../rpc.ts'
import type { RuntimeService } from '../runtime/index.ts'
import { TextSearch } from '../search/index.ts'
import type { Settings } from '../settings.ts'
import { estimateMessages } from './compaction.ts'
import { completeChats, completeFiles } from './complete.ts'
import { loadBaseline, renderBaseline } from './instructions.ts'
import { InlineAssist } from './inline.ts'
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
const changeBaseSchema = z.object({ sessionId: z.string().min(1), path: z.string().min(1).max(4096) })
const mapGetSchema = z.object({ workspaceId: z.string().min(1), refresh: z.boolean().optional() })
const mapExplainSchema = z.object({ workspaceId: z.string().min(1) })
const searchSchema = z.object({
  workspaceId: z.string().min(1), pattern: z.string().max(2000), regex: z.boolean(), caseSensitive: z.boolean(), wholeWord: z.boolean(),
  include: z.string().max(2000), exclude: z.string().max(2000),
})
const replaceSchema = searchSchema.extend({
  replacement: z.string().max(100_000),
  files: z.array(z.object({ rootId: z.string().min(1).max(128), path: z.string().min(1).max(4096) })).max(20_000),
})
const gitRootSchema = z.object({ workspaceId: z.string().min(1), rootId: z.string().min(1).max(128) })
const gitPathsSchema = gitRootSchema.extend({ paths: z.array(z.string().min(1).max(4096)).min(1).max(20_000) })
const gitCommitSchema = gitRootSchema.extend({ message: z.string().max(100_000), stageAll: z.boolean().optional() })
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
  /** Proxy settings and the request function for model services and MCP servers. */
  network?: Network | undefined
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
  const models = new Models(settings, deps.network?.fetch)
  if (env.configureDeepSeek === true) await models.configure(DEEPSEEK_FLASH)
  const mcp = new McpManager(log, deps.network?.fetch)
  // Web MCP servers that failed may be reachable through the new proxy settings.
  deps.network?.onChange(() => { mcp.retryWebServers() })
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
      if (project === undefined) throw new RpcError('workspace-not-found', localized({ zh: '工作区不存在。', en: 'This workspace does not exist.' }))
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
  models.onChange((status) => {
    rpc.emit('models.changed', status)
    // Open chats show the default model and its context window; both may have changed.
    for (const live of sessions.values()) live.refresh()
  })
  void sweepSpill(join(env.home, 'spill')).catch((error: unknown) => { log(`[spill] sweep failed: ${String(error)}`) })
  void snapshots.sweep(SPILL_RETENTION_MS).catch((error: unknown) => { log(`[snapshots] sweep failed: ${String(error)}`) })

  const session = (id: string): Promise<ChatSession> => {
    const loaded = sessions.get(id)
    if (loaded !== undefined) return Promise.resolve(loaded)
    let pending = opening.get(id)
    if (pending === undefined) {
      if (store.get(id) === undefined) return Promise.reject(new RpcError('not-found', localized({ zh: '对话不存在。', en: 'This chat does not exist.' })))
      pending = ChatSession.open(services, id).then((opened) => { sessions.set(id, opened); return opened }).finally(() => { opening.delete(id) })
      opening.set(id, pending)
    }
    return pending
  }
  const summary = (id: string): SessionSummary => {
    const live = sessions.get(id)
    if (live !== undefined) return live.summary()
    const chat = store.get(id)
    if (chat === undefined) throw new RpcError('not-found', localized({ zh: '对话不存在。', en: 'This chat does not exist.' }))
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
    if (cwd === undefined) throw new RpcError('workspace-not-found', localized({ zh: '工作区不存在。', en: 'This workspace does not exist.' }))
    const chat = sessionId === undefined ? undefined : await session(sessionId)
    const resolved = chat?.resolved() ?? models.resolve()
    if (resolved === undefined) throw new RpcError('no-model', localized({ zh: '请先配置模型。', en: 'Configure a model first.' }))
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
    // Every chat records its mode, so a later change of the default in Settings does not reach it.
    const mode = permission === undefined ? settings.defaultPermission() : permissionSchema.parse(permission)
    const chat = await store.create({ workspaceId, cwd: directory, permission: mode })
    const created = summary(chat.id)
    rpc.emit('sessions.changed', created)
    return created
  })
  rpc.register('sessions.get', async ({ sessionId, limit }) => (await session(sessionId)).snapshot(limit))
  rpc.register('sessions.rename', async ({ sessionId, title }) => {
    const trimmed = title.trim()
    if (trimmed === '' || Buffer.byteLength(trimmed, 'utf8') > 80) throw new RpcError('invalid', localized({ zh: '标题需为 1–80 字节。', en: 'A title must be 1 to 80 bytes long.' }))
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
    if (index < 0) throw new RpcError('not-found', localized({ zh: '消息不存在。', en: 'This message does not exist.' }))
    let end = index
    // Keep the tool results that answer calls in the cut-off reply.
    while (end + 1 < file.entries.length && file.entries[end + 1]!.kind === 'toolResult') end++
    const chat = await store.create({
      workspaceId: file.chat.workspaceId, cwd: file.chat.cwd, parent: { sessionId, entryId }, entries: file.entries.slice(0, end + 1),
      title: localized({ zh: `${file.chat.title || '新对话'}（分支）`, en: `${file.chat.title || 'New chat'} (branch)` }), model: file.chat.model, permission: file.chat.permission ?? settings.defaultPermission(),
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
  rpc.register('chat.retry', async ({ sessionId }) => { await (await session(sessionId)).retry() })
  rpc.register('chat.setPermission', async ({ sessionId, mode }) => (await session(sessionId)).setPermission(permissionSchema.parse(mode)))
  rpc.register('chat.approve', async (params) => {
    const { sessionId, toolCallId, decision, reason } = approveSchema.parse(params)
    const live = sessions.get(sessionId)
    if (live === undefined) throw new RpcError('not-found', localized({ zh: '这个操作已不在等待批准。', en: 'This action is no longer waiting for approval.' }))
    await live.approve(toolCallId, decision, reason)
  })
  rpc.register('chat.changes', async (params) => {
    const { sessionId } = z.object({ sessionId: z.string().min(1) }).parse(params)
    return (await session(sessionId)).changes()
  })
  rpc.register('chat.changeBase', async (params) => {
    const { sessionId, path } = changeBaseSchema.parse(params)
    return (await session(sessionId)).changeBase(path)
  })
  rpc.register('chat.keep', async (params) => {
    const { sessionId, toolCallIds } = revertSchema.parse(params)
    await (await session(sessionId)).keep(toolCallIds)
  })
  const maps = new ProjectMaps({ projects, models, settings, directory: join(env.home, 'maps'), ripgrep: services.ripgrep, log })
  rpc.register('map.get', async (params) => {
    const { workspaceId, refresh } = mapGetSchema.parse(params)
    return maps.get(brandString<WorkspaceId>(workspaceId), refresh === true)
  })
  rpc.register('map.explain', async (params) => {
    const { workspaceId } = mapExplainSchema.parse(params)
    return maps.explain(brandString<WorkspaceId>(workspaceId))
  })

  // ── search and Git ──
  const search = new TextSearch({ projects, ripgrep: services.ripgrep })
  rpc.register('search.text', async (params) => {
    const request = searchSchema.parse(params)
    return search.search({ ...request, workspaceId: brandString<WorkspaceId>(request.workspaceId) })
  })
  rpc.register('search.replace', async (params) => {
    const request = replaceSchema.parse(params)
    return search.replace({ ...request, workspaceId: brandString<WorkspaceId>(request.workspaceId),
      files: request.files.map(file => ({ rootId: brandString<IdeRootId>(file.rootId), path: file.path })) })
  })
  const git = new GitService({ projects, runtime: deps.runtime, models, settings, log })
  const gitRoot = (params: unknown) => {
    const { workspaceId, rootId } = gitRootSchema.parse(params)
    return [brandString<WorkspaceId>(workspaceId), brandString<IdeRootId>(rootId)] as const
  }
  rpc.register('git.status', async (params) => git.status(brandString<WorkspaceId>(mapExplainSchema.parse(params).workspaceId)))
  rpc.register('git.stage', async (params) => { await git.stage(...gitRoot(params), gitPathsSchema.parse(params).paths) })
  rpc.register('git.unstage', async (params) => { await git.unstage(...gitRoot(params), gitPathsSchema.parse(params).paths) })
  rpc.register('git.discard', async (params) => { await git.discard(...gitRoot(params), gitPathsSchema.parse(params).paths) })
  rpc.register('git.commit', async (params) => {
    const { message, stageAll } = gitCommitSchema.parse(params)
    return git.commit(...gitRoot(params), message, stageAll === true)
  })
  rpc.register('git.init', async (params) => { await git.init(...gitRoot(params)) })
  rpc.register('git.message', async (params) => git.message(...gitRoot(params)))
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

  // ── editor AI ──
  const inline = new InlineAssist(models, settings, (requestId, text) => { rpc.emit('inline.progress', { requestId, text }) })
  rpc.register('inline.complete', request => inline.complete(request))
  rpc.register('inline.edit', request => inline.edit(request))
  rpc.register('inline.cancel', ({ requestId }) => { inline.cancel(requestId) })

  // ── network ──
  const proxySchema = z.object({ mode: z.enum(['system', 'off', 'manual']), url: z.string().max(2048).optional() })
  /** The choice from Settings, where a masked password stands for the stored one. */
  const proxyChoice = (network: Network, params: unknown): ProxySettings => {
    const proxy = proxySchema.parse(params)
    const choice = network.withStoredPassword(proxy.mode === 'manual' ? { mode: 'manual', url: (proxy.url ?? '').trim() } : { mode: proxy.mode })
    if (choice.mode !== 'manual') return choice
    const url = proxyUrl(choice.url)
    if (url === undefined) throw new RpcError('invalid', localized({ zh: '代理地址无效，应为 host:port 或 http://host:port。', en: 'The proxy address is invalid. Use host:port or http://host:port.' }))
    // The mask stands for the saved password only at the saved address.
    if (decodeURIComponent(new URL(url).password) === PASSWORD_MASK) throw new RpcError('invalid', localized({ zh: '代理地址已更改，请重新输入代理密码。', en: 'The proxy address changed. Enter the proxy password again.' }))
    return choice
  }
  rpc.register('network.status', () => {
    if (deps.network === undefined) throw new RpcError('unavailable', localized({ zh: '网络设置不可用。', en: 'Network settings are unavailable.' }))
    return deps.network.status(models.resolve()?.setup.baseURL)
  })
  rpc.register('network.set', async (params) => {
    const network = deps.network
    if (network === undefined) throw new RpcError('unavailable', localized({ zh: '网络设置不可用。', en: 'Network settings are unavailable.' }))
    const choice = proxyChoice(network, params)
    await settings.update((data) => { data.proxy = choice })
    network.configure(settings.get().proxy)
    return network.status(models.resolve()?.setup.baseURL)
  })
  rpc.register('network.test', async (params) => {
    const network = deps.network
    if (network === undefined) throw new RpcError('unavailable', localized({ zh: '网络设置不可用。', en: 'Network settings are unavailable.' }))
    const proxy = proxyChoice(network, params)
    const resolved = models.resolve()
    if (resolved === undefined) throw new RpcError('no-model', localized({ zh: '请先配置模型，再测试连接。', en: 'Configure a model before testing the connection.' }))
    const url = resolved.setup.baseURL
    const started = Date.now()
    try {
      // Any HTTP answer, even 401 or 404, shows the service is reachable through these settings.
      const response = await network.fetchWith(proxy, url, { method: 'GET', signal: AbortSignal.timeout(15_000), redirect: 'manual' })
      await response.body?.cancel().catch(() => undefined)
      return { ok: true, url, status: response.status, ms: Date.now() - started }
    } catch (error) {
      const cause = error instanceof Error && error.cause instanceof Error ? `: ${error.cause.message}` : ''
      return { ok: false, url, ms: Date.now() - started, error: `${error instanceof Error ? error.message : String(error)}${cause}` }
    }
  })

  // ── security ──
  rpc.register('security.status', ({ workspaceId }) => securityStatus(workspaceId))
  rpc.register('security.setDefault', async ({ mode }) => {
    const next = permissionSchema.parse(mode)
    const previous = settings.defaultPermission()
    if (next !== previous) {
      // Chats from before 2.1.1 have no mode of their own; they keep the one they have been running with.
      for (const chat of store.list()) {
        if (chat.permission !== undefined) continue
        const live = sessions.get(chat.id)
        if (live === undefined) await store.setMeta(chat.id, { permission: previous })
        else await live.setPermission(previous)
      }
    }
    await settings.setDefaultPermission(next)
    for (const live of sessions.values()) live.refresh()
    const status = securityStatus(undefined)
    rpc.emit('security.changed', { defaultMode: status.defaultMode })
    return status
  })
  rpc.register('security.removeRule', async (params) => {
    const { workspaceId, kind, rule } = removeRuleSchema.parse(params)
    await settings.setPermissionRule(workspaceId, kind, rule, false)
    return securityStatus(brandString<WorkspaceId>(workspaceId))
  })

  // ── skills and MCP ──
  rpc.register('extensions.status', async ({ cwd }) => { lastExtensionsCwd = cwd; return extensionsStatus(cwd) })
  rpc.register('extensions.saveServer', ({ server, previousName }) => saveServers((servers) => {
    if (server.name !== previousName && servers.some(item => item.name === server.name)) throw new RpcError('duplicate', localized({ zh: `已存在名为 ${server.name} 的 MCP 服务器。`, en: `An MCP server named ${server.name} already exists.` }))
    const index = servers.findIndex(item => item.name === (previousName ?? server.name))
    return index < 0 ? [...servers, server] : servers.map((item, position) => position === index ? server : item)
  }))
  rpc.register('extensions.removeServer', ({ name }) => saveServers(servers => servers.filter(item => item.name !== name)))
  rpc.register('extensions.addIda', () => {
    if (env.idaMcpCommand === undefined) throw new RpcError('unavailable', localized({ zh: '未找到 uvx，无法启动 IDA MCP。', en: 'uvx was not found, so IDA MCP cannot start.' }))
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
      inline.close()
      maps.close()
      search.close()
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
