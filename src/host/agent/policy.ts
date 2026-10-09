/** Request admission, output retention, the profile global prompt and opt-in tool visibility for Rainy. */
import type { Context, Volatile } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { CONTEXT_WINDOW_EXCEEDED_CODE } from '@deepseek-ai/dsh-llm'
import type { GenerateOptions, StreamChunk } from '@deepseek-ai/dsh-llm'
import type { Agent } from '@deepseek-ai/dsh-agent'
import type {} from '@deepseek-ai/dsh-system-prompt'
import type {} from '@deepseek-ai/dsh-tools'
import type {} from '@deepseek-ai/dsh-spill'
import type {} from '@deepseek-ai/dsh-token-meter'
import type {} from '@deepseek-ai/dsh-config-editor'
import type {} from '@deepseek-ai/dsh-sandbox-policy'
import type {} from '@deepseek-ai/dsh-settings'
import type {} from './project-roots.ts'
import { realpathSync } from 'node:fs'
import {
  CalibratedCounter,
  EndpointCounter,
  estimateRequest,
  estimateText,
  promptBreakdown,
  RequestQueue,
  resolveBudget,
} from './budget.ts'
import type { Budget, PromptBreakdown, TokenCount, TokenCounter } from './budget.ts'
import type RainyCompaction from './compaction.ts'
import { compactionThreshold } from './compaction.ts'
import { previewBudget } from './budget-preview.ts'
import type { BudgetPreview } from './budget-preview.ts'
import { checkedGlobalPrompt, GLOBAL_PROMPT_ORDER, GLOBAL_PROMPT_SECTION, parseGlobalPrompt, saveGlobalPrompt } from './global-prompt.ts'
import type { GlobalPromptSettings } from './global-prompt.ts'

/** Last admission result shown by the desktop; no prompt or credential text is retained here. */
export interface BudgetSnapshot extends Budget, TokenCount {
  sessionId: string
  provider: string
  model: string
  compacting: boolean
  breakdown?: PromptBreakdown
  error?: string
}
export interface RainyState {
  budgets: Map<string, BudgetSnapshot>
  auxiliaryBudgets: Map<string, BudgetSnapshot>
  tools: Map<string, Set<string>>
  extensionDescriptions: Map<string, readonly string[]>
  extensionBudgets: Map<string, { maxTokens: number; inputRatio: number }>
  modelActivity: { activeRequests: number; lastFinishedAt: number }
  /** @param query Selected project and optional draft/model. @returns A read-only estimate before request admission. */
  previewBudget(query: unknown): Promise<BudgetPreview>
  /** @returns The saved global prompt and its configured length limit. */
  globalPrompt(): GlobalPromptSettings
  /**
   * Validate and persist the global prompt in this plugin's profile entry; later prompt assemblies use it without a restart.
   * @param raw Authenticated settings request `{ text }`; empty text removes the prompt.
   * @returns The saved prompt after Loader reconciliation.
   */
  saveGlobalPrompt(raw: unknown): Promise<GlobalPromptSettings>
}
declare module '@deepseek-ai/cordis' {
  interface Context {
    rainy: RainyState
  }
}
export interface Config {
  /** Explicit whole-request tokenizer endpoints keyed by provider id. */
  tokenizers?: Record<string, string>
  /** Provider aliases sharing one model server use the same queue key. */
  endpointGroups?: Record<string, string>
  /**
   * Concurrent requests admitted per loopback queue, whose base URL host is localhost, *.localhost, 127.0.0.1 or [::1].
   * An `endpointGroups` queue is loopback when any member provider's base URL is.
   */
  localEndpointConcurrency: number
  /** Concurrent requests admitted per other queue, including a provider without a parsable base URL. */
  remoteEndpointConcurrency: number
  /** Overrides both defaults per queue key: the provider's `endpointGroups` name, else its base-URL origin, else its id. */
  endpointConcurrency?: Record<string, number>
  /** Maximum tokens describing explicitly attached project directories. */
  rootManifestTokens?: number
  /**
   * User-authored instructions rendered as one system-prompt section for every Agent, so main turns of all sessions
   * receive them through the logged system prompt. Auxiliary requests (compaction summaries, project memory,
   * titles, connection probes) keep their own task prompts. Empty adds nothing. Saved by the settings page.
   */
  globalPrompt: Volatile<string>
  /** Maximum `globalPrompt` length in UTF-16 code units; a longer configured prompt fails load and prompt assembly. */
  globalPromptMaxChars: number
}
export const name = 'rainy-policy'
export const inject = [
  'llm',
  'agents',
  'tools',
  'systemPrompt',
  'compaction',
  'tokenMeter',
  'spillStore',
  'configEditor',
  'sandboxPolicy',
  'rainyProjectRoots',
  'workspaceRegistry',
  'agentDefaultModel',
]
/** Validated admission limits and the live global prompt. */
export const Config = z.object({
  tokenizers: z.dict(z.string()),
  endpointGroups: z.dict(z.string()),
  localEndpointConcurrency: z.number().min(1).step(1).default(1),
  remoteEndpointConcurrency: z.number().min(1).step(1).default(4),
  endpointConcurrency: z.dict(z.number().min(1).step(1)),
  rootManifestTokens: z.number().min(128).step(1).default(1024),
  globalPrompt: z.string().default('').volatile(),
  // The settings request body is capped at 64 KiB; this bound keeps a full prompt within it.
  globalPromptMaxChars: z.number().min(1).max(10000).step(1).default(4000),
})
const CORE = new Set(['read', 'write', 'edit', process.platform === 'win32' ? 'pwsh' : 'bash'])
const descriptions: Record<string, string> = {
  read: 'Read a file or a bounded line range. Use small ranges for large files.',
  write: 'Create or replace a file. Inspect existing files before overwriting.',
  edit: 'Apply an exact text replacement to a previously read file.',
  bash: 'Run a command in the project Bash shell. Use rg for search; inspect exit status.',
  pwsh: 'Run a command in the project PowerShell shell. Use rg for search; inspect exit status.',
}

function loopback(url: URL | null): boolean {
  return url !== null && (['localhost', '127.0.0.1', '[::1]'].includes(url.hostname) || url.hostname.endsWith('.localhost'))
}

/** Provider ids such as `constructor` must not read inherited Object members from a config record. */
function own<T>(record: Record<string, T> | undefined, key: string): T | undefined {
  return record !== undefined && Object.hasOwn(record, key) ? record[key] : undefined
}

/** Mount scoped guards and count the immutable final request immediately before the provider runs. */
export function apply(ctx: Context, config: Config): void {
  const globalPrompt = (): string => checkedGlobalPrompt(config.globalPrompt.get(), config.globalPromptMaxChars)
  globalPrompt()
  const state: RainyState = {
    budgets: new Map(),
    auxiliaryBudgets: new Map(),
    tools: new Map(),
    extensionDescriptions: new Map(),
    extensionBudgets: new Map(),
    modelActivity: { activeRequests: 0, lastFinishedAt: 0 },
    previewBudget: query => previewBudget(ctx, query),
    globalPrompt: () => ({ text: config.globalPrompt.get(), maxChars: config.globalPromptMaxChars }),
    saveGlobalPrompt: async (raw) => {
      const text = parseGlobalPrompt(raw, config.globalPromptMaxChars)
      const entry = ctx.fiber.entry
      if (entry === undefined) throw new Error('当前配置档案无法保存全局提示词。')
      await saveGlobalPrompt(ctx.configEditor, entry, text)
      return { text, maxChars: config.globalPromptMaxChars }
    },
  }
  ctx.provide('rainy', state)
  ctx.inject(['settings'], (child) => { child.effect(() => child.settings.configure({ auto: false }, ctx.fiber)) })
  ctx.effect(() =>
    ctx.systemPrompt.section({
      name: GLOBAL_PROMPT_SECTION,
      order: GLOBAL_PROMPT_ORDER,
      interpolate: false,
      text: () => globalPrompt(),
    }),
  )
  const queue = new RequestQueue()
  const counters = new Map<string, CalibratedCounter>()
  const overflowAttempts = new WeakMap<Agent, number>()
  const compaction = ctx.compaction as RainyCompaction
  const allowed = (name: string, agent?: Agent) =>
    CORE.has(name) || (agent !== undefined && state.tools.get(agent.session.header.id)?.has(name) === true)

  /** Read the queue key and concurrency limit from the provider profiles current at admission. */
  const admission = (provider: string): { key: string; limit: number } => {
    const entry = ctx.configEditor.entries().find(item => item.options.id === 'llm-pi-ai')
    const rawConfig: unknown = entry?.options.config
    const profiles: unknown =
      rawConfig !== null && typeof rawConfig === 'object' && 'providers' in rawConfig
        ? rawConfig.providers
        : undefined
    const endpoint = (id: string): URL | null => {
      const configured =
        profiles !== null && typeof profiles === 'object'
          ? (profiles as Record<string, unknown>)[id]
          : undefined
      return configured !== null &&
        typeof configured === 'object' &&
        'baseURL' in configured &&
        typeof configured.baseURL === 'string'
        ? URL.parse(configured.baseURL)
        : null
    }
    const group = own(config.endpointGroups, provider)
    const key = group ?? endpoint(provider)?.origin ?? provider
    const members = group === undefined
      ? [provider]
      : Object.entries(config.endpointGroups ?? {}).filter(([, name]) => name === group).map(([id]) => id)
    const local = members.some(id => loopback(endpoint(id)))
    return {
      key,
      limit: own(config.endpointConcurrency, key) ?? (local ? config.localEndpointConcurrency : config.remoteEndpointConcurrency),
    }
  }

  const additionalRoots = (cwd: string | undefined): string[] => {
    if (!cwd) return []
    const roots = ctx.rainyProjectRoots
      .forSessionCwd(cwd)
      .filter(root => !root.primary)
      .map(root => root.path)
    for (const path of roots) {
      let current: string
      try { current = realpathSync.native(path) }
      catch (error) {
        if (error instanceof Error && 'code' in error && (error.code === 'ENOENT' || error.code === 'ENOTDIR'))
          throw new Error(`已附加的项目目录不存在：${path}。请重新添加或移除该目录。`)
        throw error
      }
      if (process.platform === 'win32' ? current.toLowerCase() !== path.toLowerCase() : current !== path)
        throw new Error('已附加的项目目录现在指向其他位置，请重新添加该目录。')
    }
    if (estimateText(JSON.stringify(roots)) > (config.rootManifestTokens ?? 1024))
      throw new Error('项目目录清单超过上下文预算，请减少附加目录或调整目录清单预算。')
    return roots
  }
  ctx.effect(() => ctx.sandboxPolicy.registerWritableRoots(session => additionalRoots(session.header.cwd)))
  ctx.effect(() =>
    ctx.systemPrompt.section({
      name: 'rainy:project-directories',
      order: 4000,
      text: ({ agent }) => {
        if (!agent) return ''
        const roots = additionalRoots(agent.session.header.cwd)
        return roots.length
          ? `Additional project directories: ${JSON.stringify(roots)}. Use their absolute paths; the session working directory stays unchanged.`
          : ''
      },
    }),
  )

  ctx.effect(() =>
    ctx.tools.guard(exec => (allowed(exec.name, exec.agent) ? undefined : '该工具未在当前 RainyAgent 会话中启用。')),
  )
  ctx.on(
    'system-prompt/assemble',
    async (_assembly, context, next) => {
      const assembly = await next()
      return {
        ...assembly,
        tools: assembly.tools
          .filter(tool => allowed(tool.name, context.agent))
          .map(tool => ({
            ...tool,
            description: descriptions[tool.name] ?? tool.description,
          })),
      }
    },
    { prepend: true },
  )

  ctx.on(
    'agent/request',
    async ({ signal }, next) => {
      const request = await next()
      const info = await ctx.llm.resolveModelInfo(request.provider, request.model, signal)
      const output = info.defaultMaxTokens ?? resolveBudget(info.context?.contextWindow ?? 32768).outputTokens
      // Re-resolve after a model switch: a previous provider's output cap must not leak into a smaller window.
      return { ...request, maxTokens: resolveBudget(info.context?.contextWindow ?? 32768, output).outputTokens }
    },
    { prepend: true },
  )

  ctx.on(
    'agent/pre-step',
    async ({ agent, signal }, next) => {
      const decision = await next()
      if (decision.kind === 'reject') return decision
      const route = agent.session.requestHeader()?.config
      if (!route) return decision
      const info = await ctx.llm.resolveModelInfo(route.provider, route.model, signal)
      const budget = resolveBudget(info.context?.contextWindow ?? 32768, route.maxTokens)
      const messages = [...agent.session.deriveMessages(), ...decision.messages]
      const estimated = estimateRequest({ messages, tools: agent.session.requestHeader()?.tools })
      if (estimated >= compactionThreshold(budget)) {
        const snapshot = state.budgets.get(agent.session.header.id)
        if (snapshot) snapshot.compacting = true
        try {
          await compaction.reduce(agent, signal)
        } finally {
          if (snapshot) snapshot.compacting = false
        }
      }
      return decision
    },
    { prepend: true },
  )

  ctx.on('agent/request-error', async ({ agent, failure, signal }, next) => {
    if (failure.code !== CONTEXT_WINDOW_EXCEEDED_CODE || signal.aborted) return next()
    if ((overflowAttempts.get(agent) ?? 0) >= 1) return next()
    overflowAttempts.set(agent, 1)
    const reduced = await compaction.compactIfNeeded(agent, 'context-overflow', signal)
    return reduced === null ? next() : { kind: 'retry' }
  })
  ctx.on('agent/status', ({ agent, status }) => {
    if (status === 'idle') overflowAttempts.delete(agent)
  })

  ctx.on(
    'llm/stream',
    async function* (request: GenerateOptions, next): AsyncIterable<StreamChunk> {
      const info = await ctx.llm.resolveModelInfo(request.provider, request.model, request.signal)
      const budget = resolveBudget(info.context?.contextWindow ?? 32768, request.maxTokens ?? info.defaultMaxTokens)
      const key = `${request.provider}/${request.model}`
      let calibrated = counters.get(key)
      if (!calibrated) {
        calibrated = new CalibratedCounter()
        counters.set(key, calibrated)
      }
      const tokenizer = own(config.tokenizers, request.provider)
      const counter: TokenCounter = tokenizer ? new EndpointCounter(tokenizer, calibrated) : calibrated
      const count = await counter.count(request, request.signal)
      const id = request.sessionId ?? 'diagnostic'
      const snapshot: BudgetSnapshot = {
        ...budget,
        ...count,
        sessionId: id,
        provider: request.provider,
        model: request.model,
        compacting: request.purpose === 'compaction',
        breakdown: promptBreakdown(
          request,
          state.extensionDescriptions.get(id),
          ctx.get('rainyMemory')?.recallMessageIds(id),
          [config.globalPrompt.get().trim()],
        ),
      }
      const snapshots = request.purpose === 'project-memory' ? state.auxiliaryBudgets : state.budgets
      snapshots.delete(id)
      snapshots.set(id, snapshot)
      const extensionBudget = state.extensionBudgets.get(id)
      if (extensionBudget && request.purpose === undefined) {
        const schemas = request.tools?.filter(tool => state.tools.get(id)?.has(tool.name)) ?? []
        const extensionTokens = (snapshot.breakdown?.extensions ?? 0) + estimateText(JSON.stringify(schemas))
        const limit = Math.min(extensionBudget.maxTokens, Math.floor(budget.inputLimit * extensionBudget.inputRatio))
        if (extensionTokens > limit) {
          const message = `Skills/MCP 说明与工具定义约 ${extensionTokens} tokens，超过当前模型 ${limit} tokens 的扩展预算；请减少启用项。`
          snapshot.error = message
          yield { type: 'finish', reason: { kind: 'error', failure: { code: 'EXTENSION_BUDGET_EXCEEDED', message } } }
          return
        }
      }
      if (count.tokens > budget.inputLimit) {
        const message = `请求输入 ${count.tokens} tokens（${count.kind === 'exact' ? '实测' : '估计'}）超过 ${budget.inputLimit} 的输入预算；请压缩会话或减少附件。`
        snapshot.error = message
        yield { type: 'finish', reason: { kind: 'error', failure: { code: CONTEXT_WINDOW_EXCEEDED_CODE, message } } }
        return
      }
      const lane = admission(request.provider)
      const release = await queue.acquire(lane.key, lane.limit, request.signal)
      state.modelActivity.activeRequests++
      try {
        for await (const chunk of next()) {
          if (chunk.type === 'usage') {
            const actualInput =
              chunk.usage.inputTokens + (chunk.usage.cacheReadTokens ?? 0) + (chunk.usage.cacheWriteTokens ?? 0)
            calibrated.observe(count.tokens, actualInput)
          }
          yield chunk
        }
      } finally {
        snapshot.compacting = false
        state.modelActivity.activeRequests--
        state.modelActivity.lastFinishedAt = Date.now()
        release()
      }
    },
    { prepend: true },
  )

  ctx.on(
    'tools/post-execute',
    async (exec, result, next) => {
      const decision = await next()
      if (decision.kind !== 'accept' || 'value' in decision || !exec.agent || exec.name === 'read') return decision
      const content = decision.content ?? result.content
      if (content.some(block => block.type !== 'text')) return decision
      const text = content.map(block => (block.type === 'text' ? block.text : '')).join('\n')
      const route = exec.agent.session.requestHeader()?.config
      const info = route ? await ctx.llm.resolveModelInfo(route.provider, route.model) : undefined
      const cap = resolveBudget(info?.context?.contextWindow ?? 32768, route?.maxTokens).toolTokens
      if (estimateText(text) <= cap) return decision
      const saved = await ctx.spillStore.saveText({
        owner: { sessionId: exec.agent.session.header.id },
        source: { kind: 'tool', toolName: exec.name, callId: exec.callId, label: 'result' },
        suggestedName: `${exec.name}.txt`,
        content: text,
      })
      const notice = `\n[Output limited; full result: ${saved.locator}]\n${saved.retrievalHint}\n`
      const chars = Array.from(text)
      let keep = Math.min(chars.length, cap * 3)
      let bounded = ''
      do {
        bounded =
          chars.slice(0, Math.floor(keep * 0.75)).join('') +
          notice +
          chars.slice(chars.length - Math.floor(keep * 0.25)).join('')
        keep = Math.floor(keep * 0.8)
      } while (estimateText(bounded) > cap && keep > 0)
      if (estimateText(bounded) > cap) throw new Error('结果引用本身超过工具输出预算。')
      return { ...decision, content: [{ type: 'text', text: bounded }] }
    },
    { prepend: true },
  )
}
