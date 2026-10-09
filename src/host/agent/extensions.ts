/** Per-session, explicitly selected local skills and MCP tools. */
import { Context, Service } from '@deepseek-ai/cordis'
import type { Fiber } from '@deepseek-ai/cordis'
import { assembleContextFor, type Agent } from '@deepseek-ai/dsh-agent'
import { SessionId } from '@deepseek-ai/dsh-session'
import * as Mcp from '@deepseek-ai/dsh-mcp-client'
import { mkdir, readFile, readdir, realpath, writeFile, rename, stat } from 'node:fs/promises'
import { join, relative, isAbsolute } from 'node:path'
import { homedir } from 'node:os'
import { randomUUID } from 'node:crypto'
import { existsSync } from 'node:fs'
import s from '@deepseek-ai/schemastery'
import { load as loadYaml } from 'js-yaml'
import { estimateText, resolveBudget } from './budget.ts'
import type {} from '@deepseek-ai/dsh-tools'
import type {} from './policy.ts'

type ServerChoice = { serverName: string; tools: string[] } & (
  { transport: 'stdio'; command: string; args?: string[] } | { transport: 'streamable-http'; url: string }
)
export interface ExtensionSelection {
  skills: string[]
  servers: ServerChoice[]
  ida?: boolean
}
interface SkillChoice {
  id: string
  title: string
  path: string
}
interface ActiveSelection {
  selection: ExtensionSelection
  dispose: () => Promise<void>
}

/** Explicit limits for the descriptions and instructions of selected extensions. */
export interface ExtensionConfig {
  skillSourceBytes: number
  skillDescriptionBytes: number
  instructionTokens: number
  inputRatio: number
  mcpInstructionBytes: number
}

/**
 * Read only purpose metadata into the prompt; the existing read tool loads the full skill when needed.
 * @param body Selected local SKILL.md contents.
 * @param id Stable skill identity.
 * @param path Resolved path supplied to the existing file reader.
 * @param maxBytes Description's UTF-8 limit.
 * @returns A compact, explicitly retrievable skill descriptor.
 */
export function skillDescriptor(body: string, id: string, path: string, maxBytes: number): string {
  const front = /^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/.exec(body)
  let description = ''
  if (front) {
    const metadata: unknown = loadYaml(front[1])
    if (
      metadata !== null &&
      typeof metadata === 'object' &&
      'description' in metadata &&
      typeof metadata.description === 'string'
    )
      description = metadata.description.trim()
  }
  if (!description)
    description =
      body
        .replace(/^---\r?\n[\s\S]*?\r?\n---/, '')
        .split(/\r?\n\s*\r?\n/)
        .find(part => part.trim() && !part.trim().startsWith('#'))
        ?.trim() ?? id
  let bounded = ''
  for (const character of description.replace(/\s+/g, ' ')) {
    if (Buffer.byteLength(bounded + character, 'utf8') > maxBytes) break
    bounded += character
  }
  return `Skill ${JSON.stringify(id)}: ${bounded}\nRead ${JSON.stringify(path)} for the full instructions when this skill is relevant.`
}

/** Build the installed official Windows MCP launcher; WSL passes its arguments without a shell.
 * @param command Windows uvx executable mapped into WSL.
 * @returns The official server and its six public tools.
 */
export function officialIdaServer(command: string | undefined): ServerChoice {
  if (!command || !isAbsolute(command)) throw new Error('未找到 Windows uvx，请安装官方 IDA MCP 后重启 RainyAgent。')
  return {
    serverName: 'ida',
    transport: 'stdio',
    command,
    args: ['--offline', '--from', 'ida-mcp==20260924.0.3', 'ida-mcp', 'stdio', '--agent=rainy-agent'],
    tools: ['open_database', 'execute_python', 'reference', 'list_databases', 'save_database', 'close_database'],
  }
}
declare module '@deepseek-ai/cordis' {
  interface Context {
    rainyExtensions: RainyExtensions
  }
}

function skillRoot(agent: Agent, scope: 'project' | 'user'): string {
  return scope === 'project'
    ? join(agent.session.header.cwd ?? homedir(), '.rainy', 'skills')
    : join(homedir(), '.rainy-agent', 'skills')
}

function stringList(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item: unknown) => typeof item === 'string')
}

/** Only explicit executable/URL values and explicit tool allowlists cross from the settings UI. */
export function parseExtensions(value: unknown): ExtensionSelection {
  if (value === null || typeof value !== 'object') throw new Error('扩展配置必须是对象。')
  const data = value as Record<string, unknown>
  if (!stringList(data.skills)) throw new Error('skills 必须是名称列表。')
  if (!Array.isArray(data.servers)) throw new Error('servers 必须是列表。')
  if (data.ida !== undefined && typeof data.ida !== 'boolean') throw new Error('ida 必须是布尔值。')
  if (data.mode !== undefined && data.mode !== 'general' && data.mode !== 'ctf') throw new Error('旧版 mode 配置无效。')
  if (data.skills.length > 16 || data.servers.length > 8)
    throw new Error('一次最多启用 16 个 Skills 和 8 个 MCP 服务。')
  const names = new Set<string>()
  const servers = data.servers.map((raw): ServerChoice => {
    if (raw === null || typeof raw !== 'object') throw new Error('MCP 服务配置无效。')
    const server = raw as Record<string, unknown>
    if (
      typeof server.serverName !== 'string' ||
      !/^[A-Za-z0-9_-]{1,32}$/.test(server.serverName) ||
      names.has(server.serverName)
    )
      throw new Error('MCP 服务名无效或重复。')
    names.add(server.serverName)
    if (
      !stringList(server.tools) ||
      !server.tools.length ||
      !server.tools.every(tool => /^[A-Za-z0-9_.-]{1,128}$/.test(tool))
    )
      throw new Error('请显式列出要启用的 MCP 工具名称。')
    if (server.transport === 'stdio') {
      if (typeof server.command !== 'string' || !server.command.trim())
        throw new Error('stdio 服务需要 WSL 中的命令路径。')
      if (server.args !== undefined && !stringList(server.args)) throw new Error('MCP args 必须是字符串列表。')
      return {
        serverName: server.serverName,
        transport: 'stdio',
        tools: server.tools,
        command: server.command,
        args: server.args,
      }
    }
    if (server.transport !== 'streamable-http' || typeof server.url !== 'string') throw new Error('MCP 传输类型无效。')
    const url = new URL(server.url)
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password)
      throw new Error('MCP URL 必须是 HTTP(S) 地址。')
    return { serverName: server.serverName, transport: 'streamable-http', tools: server.tools, url: url.href }
  })
  if (data.ida === true && names.has('ida')) throw new Error('官方 IDA MCP 与自定义服务器不能同时使用 ida 名称。')
  return { skills: [...new Set(data.skills)], servers, ...(data.ida === undefined ? {} : { ida: data.ida }) }
}

/** Own selection files and scoped registrations; an empty selection starts no external process. */
export default class RainyExtensions extends Service {
  static inject = ['agents', 'rainy', 'tools', 'systemPrompt', 'llm']
  static Config: s<ExtensionConfig> = s.object({
    skillSourceBytes: s.number().min(1024).step(1).default(262144),
    skillDescriptionBytes: s.number().min(64).step(1).default(512),
    instructionTokens: s.number().min(128).step(1).default(4096),
    inputRatio: s.number().min(0.01).max(1).default(0.2),
    mcpInstructionBytes: s.number().min(256).step(1).default(8192),
  })
  private readonly root = join(process.env.RAINY_HOME ?? join(homedir(), '.rainy-agent'), 'extensions')
  private readonly active = new Map<string, ActiveSelection>()
  constructor(
    ctx: Context,
    private readonly config: ExtensionConfig,
  ) {
    super(ctx, 'rainyExtensions')
    ctx.on('agent/created', async ({ agent }) => {
      const selected = await this.readSelection(agent.id)
      if (selected.skills.length || selected.servers.length || selected.ida)
        this.active.set(agent.id, await this.mount(agent, selected))
    })
    // Scoped MCP fibers and prompt sections end with the agent context; the shared maps are cleared here.
    ctx.on('agent/disposed', ({ agent }) => {
      this.active.delete(agent.id)
      ctx.rainy.tools.delete(agent.id)
      ctx.rainy.extensionDescriptions.delete(agent.id)
      ctx.rainy.extensionBudgets.delete(agent.id)
    })
    ctx.effect(() => async () => {
      const active = [...this.active.values()]
      this.active.clear()
      const results = await Promise.allSettled(active.map(item => item.dispose()))
      const errors = results.filter((result): result is PromiseRejectedResult => result.status === 'rejected')
      if (errors.length)
        throw new AggregateError(
          errors.map((error): unknown => error.reason),
          'Extension shutdown failed',
        )
    })
  }

  private path(id: string): string {
    if (!/^[a-zA-Z0-9_.-]{1,160}$/.test(id)) throw new Error('会话 ID 无效。')
    return join(this.root, id + '.json')
  }
  private async readSelection(id: string): Promise<ExtensionSelection> {
    try {
      return parseExtensions(JSON.parse(await readFile(this.path(id), 'utf8')))
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code === 'ENOENT') return { skills: [], servers: [] }
      throw error
    }
  }
  private agent(id: string): Agent {
    const agent = this.ctx.agents.get(SessionId(id))
    if (!agent) throw new Error('请先打开一个会话。')
    return agent
  }

  /** List session identities and local skill files without loading skill text or starting MCP. */
  async catalog(id: string): Promise<{ skills: SkillChoice[]; selection: ExtensionSelection; idaAvailable: boolean }> {
    const agent = this.agent(id)
    const skills: SkillChoice[] = []
    for (const prefix of ['project', 'user'] as const) {
      const folder = skillRoot(agent, prefix)
      let children
      try {
        children = await readdir(folder, { withFileTypes: true })
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue
        throw error
      }
      for (const child of children) {
        if (!child.isDirectory()) continue
        const path = join(folder, child.name, 'SKILL.md')
        try {
          if (!(await stat(path)).isFile()) continue
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue
          throw error
        }
        skills.push({ id: prefix + '/' + child.name, title: child.name, path })
      }
    }
    const command = process.env.RAINY_IDA_MCP_COMMAND
    return { skills, selection: await this.readSelection(id), idaAvailable: !!command && existsSync(command) }
  }

  /** Enable or remove the official IDA connection while preserving other saved extensions.
   * @param id Idle session to update.
   * @param enabled Whether the session uses official IDA tools.
   * @returns The persisted selection after connection succeeds.
   */
  async setIda(id: string, enabled: boolean): Promise<ExtensionSelection> {
    return this.select(id, { ...(await this.readSelection(id)), ida: enabled })
  }

  private async mount(agent: Agent, selection: ExtensionSelection): Promise<ActiveSelection> {
    const fibers: Fiber[] = []
    const effects: Array<() => void> = []
    const tools = new Set<string>()
    const previous = this.ctx.rainy.tools.get(agent.id)
    const dispose = async () => {
      for (const effect of effects.reverse()) effect()
      for (const fiber of fibers.reverse()) await fiber.dispose()
      if (this.ctx.rainy.tools.get(agent.id) === tools) this.ctx.rainy.tools.delete(agent.id)
      this.ctx.rainy.extensionDescriptions.delete(agent.id)
      this.ctx.rainy.extensionBudgets.delete(agent.id)
    }
    try {
      let skillTokens = 0
      for (const id of selection.skills) {
        const match = /^(project|user)\/([^/\\]+)$/.exec(id)
        if (!match || match[2] === '.' || match[2] === '..') throw new Error('Skill 名称无效。')
        const root = skillRoot(agent, match[1] === 'project' ? 'project' : 'user')
        const path = await realpath(join(root, match[2], 'SKILL.md'))
        const within = relative(await realpath(root), path)
        if (isAbsolute(within) || within.startsWith('..')) throw new Error('Skill 路径超出配置目录。')
        const body = await readFile(path, 'utf8')
        if (Buffer.byteLength(body) > this.config.skillSourceBytes)
          throw new Error('Skill 文件超过读取上限；请精简后再启用。')
        const descriptor = skillDescriptor(body, id, path, this.config.skillDescriptionBytes)
        skillTokens += estimateText(descriptor)
        if (skillTokens > this.config.instructionTokens)
          throw new Error('已选择的 Skills 描述超过上下文预算；请减少启用项。')
        effects.push(
          agent.ctx.systemPrompt.section({
            name: 'rainy-skill:' + id,
            order: 6000,
            text: descriptor,
            interpolate: false,
          }),
        )
      }
      const servers = selection.ida
        ? [...selection.servers, officialIdaServer(process.env.RAINY_IDA_MCP_COMMAND)]
        : selection.servers
      for (const server of servers) {
        const { tools: selected, ...connection } = server
        const fiber = await agent.ctx.plugin(Mcp, {
          ...connection,
          maxInstructionBytes: this.config.mcpInstructionBytes,
          failOnStartupError: true,
        })
        fibers.push(fiber)
        const available = new Set(agent.ctx.tools.schemas(agent).map(tool => tool.name))
        for (const raw of selected) {
          const name = raw.startsWith('mcp__') ? raw : `mcp__${server.serverName}__${raw}`
          if (!available.has(name)) throw new Error(`MCP 服务未提供工具 ${name}`)
          tools.add(name)
        }
      }
      this.ctx.rainy.tools.set(agent.id, tools)
      const assembly = await agent.ctx.systemPrompt.assemble(assembleContextFor(agent))
      const sectionTokens = assembly.sections
        .filter(section => section.name.startsWith('rainy-skill:') || section.name.startsWith('mcp:'))
        .reduce((sum, section) => sum + estimateText(section.text), 0)
      const schemaTokens = estimateText(JSON.stringify(assembly.tools.filter(tool => tools.has(tool.name))))
      const route = agent.session.requestHeader()?.config ?? agent.options
      const model =
        route.provider && route.model ? await this.ctx.llm.resolveModelInfo(route.provider, route.model) : undefined
      const inputLimit = resolveBudget(model?.context?.contextWindow ?? 32768, model?.defaultMaxTokens).inputLimit
      if (
        sectionTokens + schemaTokens >
        Math.min(this.config.instructionTokens, Math.floor(inputLimit * this.config.inputRatio))
      ) {
        throw new Error('已选择的 Skills/MCP 说明与工具定义超过此模型的扩展预算；请减少启用项。原配置已保留。')
      }
      this.ctx.rainy.extensionDescriptions.set(
        agent.id,
        assembly.sections
          .filter(section => section.name.startsWith('rainy-skill:') || section.name.startsWith('mcp:'))
          .map(section => section.text),
      )
      this.ctx.rainy.extensionBudgets.set(agent.id, {
        maxTokens: this.config.instructionTokens,
        inputRatio: this.config.inputRatio,
      })
      return { selection, dispose }
    } catch (error) {
      await dispose()
      if (previous) this.ctx.rainy.tools.set(agent.id, previous)
      throw error
    }
  }

  /** Apply an idle-session selection and persist only after all scoped registrations are ready. */
  async select(id: string, raw: unknown): Promise<ExtensionSelection> {
    const selection = parseExtensions(raw)
    const agent = this.agent(id)
    return agent.runMaintenance(async () => {
      const old = this.active.get(id)
      if (old) await old.dispose()
      this.active.delete(id)
      let replacement: ActiveSelection | undefined
      try {
        replacement = await this.mount(agent, selection)
        await mkdir(this.root, { recursive: true, mode: 0o700 })
        const temporary = this.path(id) + '.' + randomUUID() + '.tmp'
        await writeFile(temporary, JSON.stringify(selection, null, 2) + '\n', { mode: 0o600, flag: 'wx' })
        await rename(temporary, this.path(id))
        this.active.set(id, replacement)
        return selection
      } catch (error) {
        if (replacement) await replacement.dispose()
        if (old) this.active.set(id, await this.mount(agent, old.selection))
        throw error
      }
    })
  }
}
