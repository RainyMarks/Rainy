/** Explicit model setup and separate discovery, stream and tool-call probes. */
import type { Context } from '@deepseek-ai/cordis'
import { credentialRef } from '@deepseek-ai/dsh-credentials'
import { ReasoningEffortId } from '@deepseek-ai/dsh-llm'
import { claudeReasoningLevels } from '@deepseek-ai/dsh-llm-pi-ai'
import type {} from '@deepseek-ai/dsh-config-editor'
import type {} from '@deepseek-ai/dsh-agent-default-model'
import { resolveBudget } from './budget.ts'
import { inspectStrataHealth } from './strata-health.ts'

/** Connection fields for model discovery without selecting or configuring a model. */
export interface ModelDiscovery {
  provider: string
  baseURL: string
  api: 'openai-completions' | 'openai-responses' | 'anthropic-messages'
  apiKey?: string
}

export interface ModelSetup extends Omit<ModelDiscovery, 'api'> {
  model: string
  contextWindow: number
  maxTokens?: number
  api?: ModelDiscovery['api']
  local: boolean
  thinking?: 'off' | 'low' | 'high' | 'max'
  thinkingFormat?: 'openai' | 'deepseek' | 'qwen'
  maxTokensField?: 'max_tokens' | 'max_completion_tokens'
}
/** Official DeepSeek model with Rainy's API context default. Contains no credentials. */
export const DEEPSEEK_FLASH: ModelSetup = {
  provider: 'rainy-deepseek', baseURL: 'https://api.deepseek.com', model: 'deepseek-flash',
  contextWindow: 1000000, maxTokens: 393216, api: 'openai-responses', local: false,
  thinking: 'max', thinkingFormat: 'deepseek',
}
/** Claude through the official API. A 200K window keeps long sessions affordable; the model accepts up to 1M. */
export const CLAUDE_OPUS: ModelSetup = {
  provider: 'rainy-claude', baseURL: 'https://api.anthropic.com', model: 'claude-opus-5-5',
  contextWindow: 200000, maxTokens: 64000, api: 'anthropic-messages', local: false, thinking: 'high',
}
/** Claude Haiku 5.5. A 100K window keeps every request inside its lower price tier for prompts up to 100K tokens. */
export const CLAUDE_HAIKU: ModelSetup = {
  provider: 'rainy-claude-haiku', baseURL: 'https://api.anthropic.com', model: 'claude-haiku-5-5',
  contextWindow: 100000, maxTokens: 16000, api: 'anthropic-messages', local: false, thinking: 'high',
}
/** Rainy's Claude presets share one Anthropic key. */
const CLAUDE_DISPLAY_NAMES: Readonly<Record<string, string>> = { [CLAUDE_OPUS.provider]: 'Claude', [CLAUDE_HAIKU.provider]: 'Claude Haiku' }
const reasoningEfforts = { off: 'none', low: 'low', high: 'high', max: 'max' } as const

/**
 * The effort a Claude model actually runs at. Current Claude models cannot turn
 * thinking off, so Rainy's "off" becomes their lowest effort.
 * @param setup Saved or requested model settings.
 * @returns the effort to send, or the setting unchanged for a model that is not a known Claude model.
 */
export function effectiveThinking(setup: Pick<ModelSetup, 'api' | 'model' | 'thinking'>): ModelSetup['thinking'] {
  const levels = setup.api === 'anthropic-messages' ? claudeReasoningLevels(setup.model) : undefined
  return levels !== undefined && setup.thinking === 'off' && !levels.includes('off') ? 'low' : setup.thinking
}

/** Credential variable owned by Rainy for one provider; repair recognizes only these profiles. */
function credentialEnv(provider: string): string {
  if (provider === DEEPSEEK_FLASH.provider) return 'DEEPSEEK_API_KEY'
  return provider in CLAUDE_DISPLAY_NAMES ? 'ANTHROPIC_API_KEY' : `RAINY_${provider.replaceAll('-', '_').toUpperCase()}_KEY`
}

/** Display name Rainy writes for a provider it configures. */
function ownedDisplayName(provider: string, local: boolean): string {
  if (local) return '本地模型'
  return provider === DEEPSEEK_FLASH.provider ? 'DeepSeek V4.1 Flash' : CLAUDE_DISPLAY_NAMES[provider] ?? provider
}

/** Validate discovery connection fields without including credentials in diagnostics.
 * @param value Renderer or process input.
 * @returns Validated endpoint and optional request credential.
 */
export function parseModelDiscovery(value: unknown): ModelDiscovery {
  if (value === null || typeof value !== 'object') throw new Error('模型设置必须是对象。')
  const data = value as Record<string, unknown>
  if (typeof data.provider !== 'string' || !/^[a-z][a-z0-9-]{0,47}$/.test(data.provider)) throw new Error('供应商 ID 只允许小写字母、数字和连字符。')
  if (typeof data.baseURL !== 'string') throw new Error('请填写服务地址。')
  let url: URL
  try { url = new URL(data.baseURL) }
  catch (_error) { throw new Error('服务地址必须是有效的 HTTP(S) Base URL。') }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) throw new Error('服务地址必须是 HTTP(S) Base URL，不能包含密钥或查询参数。')
  if (typeof data.api !== 'string' || !['openai-completions', 'openai-responses', 'anthropic-messages'].includes(data.api)) throw new Error('不支持的 API 协议。')
  if (data.apiKey !== undefined && typeof data.apiKey !== 'string') throw new Error('密钥格式无效。')
  return { provider: data.provider, baseURL: url.href.replace(/\/$/, ''), api: data.api as ModelDiscovery['api'],
    ...(data.apiKey === undefined ? {} : { apiKey: data.apiKey }) }
}

/** Validate renderer/process data without reflecting credentials in errors. */
export function parseModelSetup(value: unknown): ModelSetup {
  if (value === null || typeof value !== 'object') throw new Error('模型设置必须是对象。')
  const data = value as Record<string, unknown>
  if (typeof data.model !== 'string' || !data.model.trim()) throw new Error('请填写服务地址和模型 ID。')
  if (typeof data.contextWindow !== 'number' || typeof data.local !== 'boolean') throw new Error('请填写实际上下文长度并选择本地或 API 模型。')
  if (data.maxTokens !== undefined && typeof data.maxTokens !== 'number') throw new Error('输出上限必须是数字。')
  resolveBudget(data.contextWindow, data.maxTokens)
  const connection = parseModelDiscovery({ ...data, api: data.api ?? (data.local ? 'openai-completions' : 'openai-responses') })
  if (data.thinking !== undefined && (typeof data.thinking !== 'string' || !['off', 'low', 'high', 'max'].includes(data.thinking))) throw new Error('推理档位无效。')
  if (data.thinkingFormat !== undefined && (typeof data.thinkingFormat !== 'string' || !['openai', 'deepseek', 'qwen'].includes(data.thinkingFormat))) throw new Error('推理协议无效。')
  if (data.maxTokensField !== undefined && (typeof data.maxTokensField !== 'string' || !['max_tokens', 'max_completion_tokens'].includes(data.maxTokensField))) throw new Error('输出参数格式无效。')
  return { ...connection, model: data.model.trim(), contextWindow: data.contextWindow,
    local: data.local,
    ...(data.maxTokens === undefined ? {} : { maxTokens: data.maxTokens }),
    ...(data.thinking === undefined ? {} : { thinking: data.thinking as ModelSetup['thinking'] }),
    ...(data.thinkingFormat === undefined ? {} : { thinkingFormat: data.thinkingFormat as ModelSetup['thinkingFormat'] }),
    ...(data.maxTokensField === undefined ? {} : { maxTokensField: data.maxTokensField as ModelSetup['maxTokensField'] }),
  }
}

/** Save a key separately, then atomically activate the provider through DSH's validated config editor. */
export async function configureModel(ctx: Context, raw: unknown): Promise<{ provider: string; model: string }> {
  const setup = parseModelSetup(raw)
  const ref = credentialRef(credentialEnv(setup.provider))
  if (setup.apiKey?.trim()) await ctx.credentials.set(ref, setup.apiKey.trim())
  else if (setup.local && !(await ctx.credentials.describe(ref)).configured) await ctx.credentials.set(ref, 'rainy-local-no-key')
  else if (!(await ctx.credentials.describe(ref)).configured) throw new Error('请填写 API 密钥。')
  const entry = ctx.configEditor.entries().find(item => item.options.id === 'llm-pi-ai')
  if (!entry) throw new Error('模型适配器尚未就绪。')
  const budget = resolveBudget(setup.contextWindow, setup.maxTokens)
  // A known Claude model keeps the installed catalog's thinking protocol and efforts,
  // and its prompt cache lives an hour so a pause to read output does not resend the session.
  const claude = setup.api === 'anthropic-messages' && claudeReasoningLevels(setup.model) !== undefined
  const thinking = effectiveThinking(setup)
  const profile = {
    displayName: ownedDisplayName(setup.provider, setup.local),
    baseURL: setup.baseURL, api: setup.api, apiKeyEnv: ref,
    defaultContextWindow: setup.contextWindow, defaultMaxTokens: budget.outputTokens,
    streamIdleTimeoutMs: 300000, timeoutMs: 1800000,
    retryPolicy: { mode: 'normal', maxRetries: 1 },
    ...(thinking === undefined ? {} : { reasoning: thinking }),
    ...(claude ? { cacheRetention: 'long' } : {}),
    ...(setup.api === 'openai-completions' ? { compat: { maxTokensField: setup.maxTokensField ?? 'max_tokens', supportsDeveloperRole: false,
      ...(setup.thinkingFormat === undefined ? {} : { thinkingFormat: setup.thinkingFormat }),
    } } : {}),
    models: [{ id: setup.model, contextWindow: setup.contextWindow, maxTokens: budget.outputTokens,
      ...(claude ? {} : setup.thinking === undefined ? { reasoningEfforts: false } : { reasoningEfforts }),
    }],
  }
  await ctx.configEditor.edit(entry, current => ({ ...current,
    providers: { ...objectRecord(objectRecord(current).providers), [setup.provider]: profile },
  }))
  await ctx.agentDefaultModel.saveSelection({ provider: setup.provider, model: setup.model,
    ...(thinking === undefined || thinking === 'off' ? {} : { reasoningEffort: ReasoningEffortId(thinking) }),
  })
  return { provider: setup.provider, model: setup.model }
}

/**
 * Select a loaded Strata endpoint only after the active Host can reach and identify it.
 * @param ctx - current execution Host and its model configuration owner.
 * @param raw - authenticated carrier request containing a local base URL.
 * @returns the saved local provider and actual model identity.
 */
export async function connectStrataModel(ctx: Context, raw: unknown): Promise<{ provider: string; model: string }> {
  if (raw === null || typeof raw !== 'object' || !('baseURL' in raw) || typeof raw.baseURL !== 'string') {
    throw new Error('Strata 连接需要本机服务地址。')
  }
  let health: Awaited<ReturnType<typeof inspectStrataHealth>>
  try { health = await inspectStrataHealth(raw.baseURL) }
  catch (error) {
    const detail = error instanceof Error ? error.message : '本地服务未就绪。'
    throw new Error(`当前执行环境无法连接已加载的 Strata：${detail} Windows 本地模型请使用 Windows 执行环境；不会切换到云端。`)
  }
  if ('expectedContextWindow' in raw && raw.expectedContextWindow !== health.contextWindow) {
    throw new Error('Strata 的实际上下文已改变，请刷新后重新连接。')
  }
  if ('maxTokens' in raw && (typeof raw.maxTokens !== 'number' || !('expectedContextWindow' in raw))) {
    throw new Error('调整输出上限需要确认当前 Strata 上下文。')
  }
  const previous = configuredModels(ctx).find(model => model.provider === 'rainy-strata')
  const maxTokens = 'maxTokens' in raw ? raw.maxTokens : previous?.maxTokens
  return configureModel(ctx, { provider: 'rainy-strata', ...health, local: true,
    api: 'openai-completions', thinkingFormat: 'openai', maxTokensField: 'max_tokens',
    thinking: previous?.thinking ?? 'off', ...(maxTokens === undefined ? {} : { maxTokens }),
  })
}

function objectRecord(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {}
}

function repairThinkingProfiles(value: unknown): { config: Record<string, unknown>; changedModels: number } {
  const config = objectRecord(value)
  let changedModels = 0
  const providers = Object.fromEntries(Object.entries(objectRecord(config.providers)).map(([provider, value]) => {
    const profile = objectRecord(value)
    const ownedName = profile.displayName === ownedDisplayName(provider, true) || profile.displayName === ownedDisplayName(provider, false)
    if (!ownedName || profile.apiKeyEnv !== credentialEnv(provider) || !Array.isArray(profile.models)) return [provider, value]
    const ids = profile.models.map(model => objectRecord(model).id)
    const [id] = ids
    if (profile.api === 'anthropic-messages' && ids.length === 1 && typeof id === 'string' && claudeReasoningLevels(id) !== undefined) {
      return [provider, repairClaudeProfile(profile, id, () => { changedModels++ })]
    }
    if (profile.reasoning !== 'off') return [provider, value]
    const models = profile.models.map((value: unknown) => {
      const model = objectRecord(value)
      if (model.reasoningEfforts !== false) return value
      changedModels++
      return { ...model, reasoningEfforts }
    })
    return [provider, { ...profile, models }]
  }))
  return { config: { ...config, providers }, changedModels }
}

/** Bring a Claude profile saved before Rainy recognized Claude to the catalog's efforts and the hour-long cache. */
function repairClaudeProfile(profile: Record<string, unknown>, id: string, changed: () => void): Record<string, unknown> {
  const models = (profile.models as unknown[]).map(objectRecord)
  const thinking = typeof profile.reasoning === 'string' && ['off', 'low', 'high', 'max'].includes(profile.reasoning)
    ? effectiveThinking({ api: 'anthropic-messages', model: id, thinking: profile.reasoning as ModelSetup['thinking'] }) : profile.reasoning
  if (profile.cacheRetention !== undefined && thinking === profile.reasoning && models.every(model => !('reasoningEfforts' in model))) return profile
  changed()
  return { ...profile, ...(thinking === undefined ? {} : { reasoning: thinking }), cacheRetention: profile.cacheRetention ?? 'long',
    models: models.map(({ reasoningEfforts: _efforts, ...model }) => model) }
}

/**
 * Repair explicit thinking-off capabilities saved by Rainy's earlier model setup.
 * @param ctx Loaded Host configuration editor, before requests are accepted.
 * @returns Number of model entries repaired; an already current profile performs no write.
 */
export async function migrateSavedModelThinking(ctx: Context): Promise<number> {
  const entry = ctx.configEditor.entries().find(item => item.options.id === 'llm-pi-ai')
  if (!entry) throw new Error('模型适配器尚未就绪。')
  if (repairThinkingProfiles(entry.options.config).changedModels === 0) return 0
  let changedModels = 0
  await ctx.configEditor.edit(entry, (current) => {
    const repaired = repairThinkingProfiles(current)
    changedModels = repaired.changedModels
    return repaired.config
  })
  return changedModels
}

/** Project only public model settings; credentials never cross the settings/status endpoint. */
export function configuredModels(ctx: Context): ModelSetup[] {
  const config = objectRecord(ctx.configEditor.entries().find(item => item.options.id === 'llm-pi-ai')?.options.config)
  const providers = objectRecord(config.providers)
  return Object.entries(providers).flatMap(([provider, value]) => {
    const profile = objectRecord(value)
    const compat = objectRecord(profile.compat)
    return (Array.isArray(profile.models) ? profile.models : []).map((model) => {
      const data = objectRecord(model)
      return parseModelSetup({ provider, baseURL: profile.baseURL, model: data.id,
        contextWindow: data.contextWindow ?? profile.defaultContextWindow, maxTokens: data.maxTokens ?? profile.defaultMaxTokens,
        local: profile.displayName === '本地模型', api: profile.api, thinking: profile.reasoning,
        thinkingFormat: compat.thinkingFormat, maxTokensField: compat.maxTokensField,
      })
    })
  })
}

/** Read model metadata without saving connection details or changing the selected model.
 * @param ctx Loaded model discovery service.
 * @param raw Connection fields; no model identifier or context allocation is required.
 * @returns Adapter discovery output. Model discovery does not verify tool support.
 */
export async function discoverModels(ctx: Context, raw: unknown): Promise<unknown> {
  const setup = parseModelDiscovery(raw)
  const data = await ctx.llm.discoverModels('llm-pi-ai', {
    provider: setup.provider, baseURL: setup.baseURL, api: setup.api,
    ...(setup.apiKey ? { apiKey: setup.apiKey } : {}),
  }, AbortSignal.timeout(10000))
  return { data }
}

/** Run one harmless stream or tool-schema probe through the actual selected adapter. */
export async function probeModel(ctx: Context, raw: unknown): Promise<{ stream: boolean; toolCall: boolean; text: string }> {
  const setup = parseModelSetup(raw)
  const saved = configuredModels(ctx).find(model => model.provider === setup.provider && model.model === setup.model)
  if (!saved || saved.baseURL !== setup.baseURL || saved.api !== setup.api
    || saved.contextWindow !== setup.contextWindow || saved.maxTokens !== resolveBudget(setup.contextWindow, setup.maxTokens).outputTokens
    || saved.thinking !== effectiveThinking(setup) || (setup.api === 'openai-completions'
      && ((saved.thinkingFormat ?? 'openai') !== (setup.thinkingFormat ?? 'openai')
        || (saved.maxTokensField ?? 'max_tokens') !== (setup.maxTokensField ?? 'max_tokens')))) throw new Error('请先保存当前模型设置，再验证这份已保存的配置。')
  const budget = resolveBudget(setup.contextWindow, setup.maxTokens)
  let text = ''; let toolCall = false; let stream = false
  const thinking = effectiveThinking(setup)
  const route = {
    provider: setup.provider, model: setup.model, maxTokens: budget.outputTokens,
    ...(thinking === undefined || thinking === 'off' ? {} : { reasoningEffort: ReasoningEffortId(thinking) }),
  }
  for await (const chunk of ctx.llm.stream({ ...route,
    messages: [{ role: 'user', content: [{ type: 'text', text: 'Connection diagnostic. Reply only with RainyAgent connected.' }] }],
    signal: AbortSignal.timeout(180000),
  })) {
    if (chunk.type === 'text-delta') { stream = true; text = (text + chunk.text).slice(0, 300) }
    if (chunk.type === 'finish' && (chunk.reason.kind === 'error' || chunk.reason.kind === 'aborted')) throw new Error(chunk.reason.failure.message)
  }
  try {
    for await (const chunk of ctx.llm.stream({ ...route,
      messages: [{ role: 'user', content: [{ type: 'text', text: 'Connection diagnostic. Call rainy_probe with value 7 exactly once. Do not access files or run commands.' }] }],
      tools: [{ name: 'rainy_probe', description: 'Harmless connection diagnostic; returns the supplied integer.', parameters: { type: 'object', properties: { value: { type: 'integer' } }, required: ['value'], additionalProperties: false } }],
      signal: AbortSignal.timeout(180000),
    })) {
      if (chunk.type === 'block-end' && chunk.block.type === 'tool-call' && chunk.block.name === 'rainy_probe') toolCall = true
      if (chunk.type === 'finish' && (chunk.reason.kind === 'error' || chunk.reason.kind === 'aborted')) throw new Error(chunk.reason.failure.message)
    }
  } catch (error) {
    text += '\n工具诊断：' + (error instanceof Error ? error.message.slice(0, 200) : '未通过')
  }
  return { stream, toolCall, text }
}
