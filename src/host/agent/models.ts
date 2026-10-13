/** Saved model profiles, presets, discovery, the connection probe and Strata connection. */
import { z } from 'zod'
import type { Api, AssistantMessage, Model } from '@earendil-works/pi-ai'
import { createInitialSystemMessage, Type } from '@earendil-works/pi-ai'
import { resolveBudget } from '../../shared/budget.ts'
import type {
  ModelDiscoveryInput, ModelSelection, ModelSetup, ModelSetupInput, ModelsStatus, ThinkingLevel,
} from '../../shared/rpc.ts'
import { localized } from '../../shared/locale.ts'
import { inspectStrataHealth } from '../../shared/strata-health.ts'
import { RpcError } from '../rpc.ts'
import { GLOBAL_PROMPT_MAX_CHARS, LOCAL_NO_KEY, type Settings } from '../settings.ts'
import { createStreamFn, effectiveThinking, reasoningOption, thinkingLevels, toPiModel } from './llm.ts'

/** Official DeepSeek API with RainyAgent's context default. */
export const DEEPSEEK_FLASH: ModelSetup = {
  provider: 'rainy-deepseek', baseURL: 'https://api.deepseek.com', model: 'deepseek-flash',
  contextWindow: 1000000, maxTokens: 393216, api: 'openai-responses', local: false, thinking: 'max', thinkingFormat: 'deepseek',
}
/** Claude through the official API; a 200K window keeps long chats affordable. */
export const CLAUDE_OPUS: ModelSetup = {
  provider: 'rainy-claude', baseURL: 'https://api.anthropic.com', model: 'claude-opus-5-5',
  contextWindow: 200000, maxTokens: 64000, api: 'anthropic-messages', local: false, thinking: 'high',
}
/** Claude Haiku 5.5; a 100K window keeps every request in its lower price tier. */
export const CLAUDE_HAIKU: ModelSetup = {
  provider: 'rainy-claude-haiku', baseURL: 'https://api.anthropic.com', model: 'claude-haiku-5-5',
  contextWindow: 100000, maxTokens: 16000, api: 'anthropic-messages', local: false, thinking: 'high',
}
const PRESETS = [
  { name: 'DeepSeek V4.1 Flash', model: DEEPSEEK_FLASH },
  { name: 'Claude Opus 5.5', model: CLAUDE_OPUS },
  { name: 'Claude Haiku 5.5', model: CLAUDE_HAIKU },
]

const api = z.enum(['openai-completions', 'openai-responses', 'anthropic-messages'])
const thinking = z.enum(['off', 'low', 'high', 'max'])

/**
 * Validate connection fields without echoing credentials in errors.
 * @param value Renderer input.
 * @returns Normalized connection fields.
 */
export function parseDiscovery(value: unknown): ModelDiscoveryInput {
  if (value === null || typeof value !== 'object') throw new Error(localized({ zh: '模型设置必须是对象。', en: 'Model settings must be an object.' }))
  const data = value as Record<string, unknown>
  if (typeof data.provider !== 'string' || !/^[a-z][a-z0-9-]{0,47}$/.test(data.provider)) throw new Error(localized({ zh: '供应商 ID 只允许小写字母、数字和连字符。', en: 'The provider ID may contain only lowercase letters, digits and hyphens.' }))
  if (typeof data.baseURL !== 'string') throw new Error(localized({ zh: '请填写服务地址。', en: 'Enter the service address.' }))
  let url: URL
  try { url = new URL(data.baseURL) } catch (_error) { throw new Error(localized({ zh: '服务地址必须是有效的 HTTP(S) Base URL。', en: 'The service address must be a valid HTTP(S) base URL.' })) }
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error(localized({ zh: '服务地址必须是 HTTP(S) Base URL，不能包含密钥或查询参数。', en: 'The service address must be an HTTP(S) base URL without credentials or query parameters.' }))
  }
  const protocol = api.safeParse(data.api)
  if (!protocol.success) throw new Error(localized({ zh: '不支持的 API 协议。', en: 'This API protocol is not supported.' }))
  if (data.apiKey !== undefined && typeof data.apiKey !== 'string') throw new Error(localized({ zh: '密钥格式无效。', en: 'The key format is invalid.' }))
  return { provider: data.provider, baseURL: url.href.replace(/\/$/, ''), api: protocol.data, ...(data.apiKey === undefined ? {} : { apiKey: data.apiKey }) }
}

/**
 * Validate a model form.
 * @param value Renderer or carrier input.
 * @returns The profile plus the optional key.
 */
export function parseSetup(value: unknown): ModelSetupInput {
  if (value === null || typeof value !== 'object') throw new Error(localized({ zh: '模型设置必须是对象。', en: 'Model settings must be an object.' }))
  const data = value as Record<string, unknown>
  if (typeof data.model !== 'string' || !data.model.trim()) throw new Error(localized({ zh: '请填写服务地址和模型 ID。', en: 'Enter the service address and the model ID.' }))
  if (typeof data.contextWindow !== 'number' || typeof data.local !== 'boolean') throw new Error(localized({ zh: '请填写实际上下文长度并选择本地或 API 模型。', en: 'Enter the actual context length and choose a local or API model.' }))
  if (data.maxTokens !== undefined && typeof data.maxTokens !== 'number') throw new Error(localized({ zh: '输出上限必须是数字。', en: 'The output limit must be a number.' }))
  resolveBudget(data.contextWindow, data.maxTokens)
  const connection = parseDiscovery({ ...data, api: data.api ?? (data.local ? 'openai-completions' : 'openai-responses') })
  if (data.thinking !== undefined && !thinking.safeParse(data.thinking).success) throw new Error(localized({ zh: '推理档位无效。', en: 'The reasoning level is invalid.' }))
  if (data.thinkingFormat !== undefined && !['openai', 'deepseek', 'qwen'].includes(String(data.thinkingFormat))) throw new Error(localized({ zh: '推理协议无效。', en: 'The reasoning format is invalid.' }))
  if (data.maxTokensField !== undefined && !['max_tokens', 'max_completion_tokens'].includes(String(data.maxTokensField))) throw new Error(localized({ zh: '输出参数格式无效。', en: 'The output limit field is invalid.' }))
  return {
    ...connection, model: data.model.trim(), contextWindow: data.contextWindow, local: data.local,
    ...(data.maxTokens === undefined ? {} : { maxTokens: data.maxTokens }),
    ...(data.thinking === undefined ? {} : { thinking: data.thinking as ThinkingLevel }),
    ...(data.thinkingFormat === undefined ? {} : { thinkingFormat: data.thinkingFormat as ModelSetup['thinkingFormat'] }),
    ...(data.maxTokensField === undefined ? {} : { maxTokensField: data.maxTokensField as ModelSetup['maxTokensField'] }),
    ...(typeof data.previousProvider === 'string' && /^[a-z][a-z0-9-]{0,47}$/.test(data.previousProvider) ? { previousProvider: data.previousProvider } : {}),
  }
}

/** A model resolved for one request. */
export interface ResolvedModel {
  setup: ModelSetup
  model: Model<Api>
  thinking: ThinkingLevel | undefined
}

/** Model settings of one Host. */
export class Models {
  /** The stream function every chat, compaction and memory request uses. */
  readonly streamFn: ReturnType<typeof createStreamFn>
  private readonly listeners = new Set<(status: ModelsStatus) => void>()

  /**
   * @param settings Settings and credential store.
   * @param request Request function honoring the proxy settings.
   */
  constructor(private readonly settings: Settings, private readonly request: typeof globalThis.fetch = globalThis.fetch) {
    this.streamFn = createStreamFn({
      apiKey: provider => settings.apiKey(provider),
      fetch: request,
      cacheRetention: model => model.api === 'anthropic-messages' ? 'long' : undefined,
    })
  }

  /**
   * Observe changes.
   * @param listener Receives the new status.
   * @returns A function that removes the listener.
   */
  onChange(listener: (status: ModelsStatus) => void): () => void {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  /** @returns Saved models, keys present, selection, presets and the global prompt. */
  status(): ModelsStatus {
    const data = this.settings.get()
    // Presets count too: a preset that shares a stored key (both Claude models, an imported DeepSeek key) saves without one.
    const providers = new Set([...data.models, ...PRESETS.map(preset => preset.model)].map(model => model.provider))
    return {
      models: data.models,
      credentials: [...providers].filter((provider) => {
        const key = this.settings.apiKey(provider)
        return key !== undefined && key !== LOCAL_NO_KEY
      }),
      keysEncrypted: this.settings.keysSealed,
      ...(this.settings.unreadableKeysMessage() === undefined ? {} : { keysUnreadable: this.settings.unreadableKeysMessage() }),
      selected: data.selected,
      thinkingLevels: Object.fromEntries(data.models.map(model => [model.provider, thinkingLevels(model)])),
      presets: PRESETS,
      globalPrompt: { text: data.globalPrompt, maxChars: GLOBAL_PROMPT_MAX_CHARS },
    }
  }

  /**
   * Resolve a selection to a streamable model.
   * @param selection Chat or default selection; `undefined` uses the default.
   * @returns The model, or `undefined` when nothing is configured.
   */
  resolve(selection?: ModelSelection | null): ResolvedModel | undefined {
    const data = this.settings.get()
    const target = selection ?? data.selected
    const setup = target === null || target === undefined ? data.models[0]
      : data.models.find(model => model.provider === target.provider && model.model === target.model)
    if (setup === undefined) return undefined
    const requested = target !== null && target !== undefined && target.provider === setup.provider && target.model === setup.model ? target.thinking ?? setup.thinking : setup.thinking
    return { setup, model: toPiModel(setup), thinking: effectiveThinking(setup, requested) }
  }

  /**
   * Save a model profile and make it the default. One profile is kept per provider id.
   * @param raw Form input.
   * @returns The new default selection.
   */
  async configure(raw: unknown): Promise<ModelSelection> {
    const input = parseSetup(raw)
    const { apiKey, previousProvider, ...setup } = input
    // Editing a profile under a new provider id moves it; the old id's key comes along unless a new one is typed.
    const renamed = previousProvider !== undefined && previousProvider !== setup.provider
      && this.settings.get().models.some(model => model.provider === previousProvider) ? previousProvider : undefined
    const carried = renamed === undefined ? undefined : this.settings.apiKey(renamed)
    const stored = this.settings.apiKey(setup.provider) ?? carried
    resolveBudget(setup.contextWindow, setup.maxTokens)
    // A local model's placeholder key is no key for an API service.
    if (!apiKey?.trim() && !setup.local && (stored === undefined || stored === LOCAL_NO_KEY)) {
      throw new RpcError('missing-key', localized({ zh: '请填写 API 密钥。API 模型需要密钥才能保存。', en: 'Enter the API key. An API model cannot be saved without one.' }))
    }
    if (apiKey?.trim()) await this.settings.setApiKey(setup.provider, apiKey.trim())
    else if (this.settings.apiKey(setup.provider) === undefined) await this.settings.setApiKey(setup.provider, stored ?? LOCAL_NO_KEY)
    const level = effectiveThinking(setup)
    // An empty output limit stays automatic, so a later context change resizes it instead of keeping the old share.
    const saved: ModelSetup = { ...setup, ...(level === undefined ? {} : { thinking: level }) }
    const data = this.settings.get()
    const previous = data.models.find(model => model.provider === saved.provider && model.model === saved.model)
    const current = data.selected
    // Re-saving the default with an unchanged form level keeps the level picked in the list.
    const kept = previous !== undefined && previous.thinking === saved.thinking && current !== null
      && current.provider === saved.provider && current.model === saved.model && current.thinking !== undefined ? effectiveThinking(saved, current.thinking) : level
    const selection: ModelSelection = { provider: saved.provider, model: saved.model, ...(kept === undefined || kept === 'off' ? {} : { thinking: kept }) }
    await this.settings.update((next) => {
      next.models = [...next.models.filter(model => model.provider !== saved.provider && model.provider !== renamed), saved]
      next.selected = selection
    })
    if (renamed !== undefined && !this.settings.get().models.some(model => credentialShared(model.provider, renamed))) await this.settings.setApiKey(renamed, undefined)
    this.publish()
    return selection
  }

  /**
   * Delete a provider's profile and key.
   * @param provider Provider id.
   * @returns The new status.
   */
  async remove(provider: string): Promise<ModelsStatus> {
    await this.settings.update((data) => {
      data.models = data.models.filter(model => model.provider !== provider)
      if (data.selected?.provider === provider) {
        const next = data.models[0]
        data.selected = next === undefined ? null : { provider: next.provider, model: next.model }
      }
    })
    const shared = this.settings.get().models.some(model => model.provider !== provider && credentialShared(model.provider, provider))
    if (!shared) await this.settings.setApiKey(provider, undefined)
    this.publish()
    return this.status()
  }

  /**
   * Choose the model future chats start with.
   * @param selection Saved provider/model and optional level.
   * @returns The new status.
   */
  async select(selection: ModelSelection): Promise<ModelsStatus> {
    const setup = this.settings.get().models.find(model => model.provider === selection.provider && model.model === selection.model)
    if (setup === undefined) throw new RpcError('not-found', localized({ zh: '模型尚未配置。', en: 'This model is not configured.' }))
    const level = effectiveThinking(setup, selection.thinking)
    await this.settings.update((data) => {
      data.selected = { provider: setup.provider, model: setup.model, ...(level === undefined ? {} : { thinking: level }) }
    })
    this.publish()
    return this.status()
  }

  /**
   * Save the text appended to every system prompt.
   * @param text Prompt text; empty removes it.
   * @returns The new status.
   */
  async setGlobalPrompt(text: string): Promise<ModelsStatus> {
    const trimmed = text.trim()
    if (trimmed.length > GLOBAL_PROMPT_MAX_CHARS) throw new RpcError('too-large', localized({ zh: `全局提示词最多 ${GLOBAL_PROMPT_MAX_CHARS} 个字符。`, en: `The global prompt can be at most ${GLOBAL_PROMPT_MAX_CHARS} characters.` }))
    await this.settings.update((data) => { data.globalPrompt = trimmed })
    this.publish()
    return this.status()
  }

  /**
   * List the models an endpoint offers. Nothing is saved.
   * @param raw Connection fields; a missing key falls back to the stored one.
   * @returns Model ids with context windows when the endpoint reports them.
   */
  async discover(raw: unknown): Promise<{ id: string; contextWindow?: number | undefined }[]> {
    const input = parseDiscovery(raw)
    const key = input.apiKey?.trim() || this.settings.apiKey(input.provider)
    const base = input.baseURL.replace(/\/+$/, '')
    const url = input.api === 'anthropic-messages' ? `${base.endsWith('/v1') ? base.slice(0, -3) : base}/v1/models?limit=1000` : `${base}/models`
    const headers: Record<string, string> = { accept: 'application/json' }
    if (input.api === 'anthropic-messages') {
      headers['anthropic-version'] = '2023-06-01'
      if (key) headers['x-api-key'] = key
    } else if (key) headers.authorization = `Bearer ${key}`
    let response: Response
    try { response = await this.request(url, { headers, signal: AbortSignal.timeout(10_000), redirect: 'error' }) } catch (error) {
      const reason = error instanceof Error ? error.message : String(error)
      throw new RpcError('discovery-failed', localized({ zh: `无法连接 ${url}：${reason}`, en: `Could not connect to ${url}: ${reason}` }))
    }
    if (!response.ok) throw new RpcError('discovery-failed', localized({ zh: `模型列表请求失败（HTTP ${response.status}）。`, en: `The model list request failed (HTTP ${response.status}).` }))
    const text = await response.text()
    if (text.length > 8 * 1024 * 1024) throw new RpcError('discovery-failed', localized({ zh: '模型列表过大。', en: 'The model list is too large.' }))
    const body: unknown = JSON.parse(text)
    const listed = isRecord(body) && Array.isArray(body.data) ? body.data
      : isRecord(body) && Array.isArray(body.models) ? body.models
        : isRecord(body) && isRecord(body.models) ? Object.entries(body.models).map(([id, entry]) => ({ id, ...(isRecord(entry) ? entry : {}) })) : undefined
    if (listed === undefined) throw new RpcError('discovery-failed', localized({ zh: '该服务的模型列表格式无法识别，请手动填写模型 ID。', en: 'This service lists its models in an unknown format. Enter the model ID yourself.' }))
    return listed.flatMap((entry) => {
      if (!isRecord(entry) || typeof entry.id !== 'string' || entry.id === '') return []
      const window = [entry.contextWindow, entry.context_window, entry.context_length, entry.max_input_tokens, isRecord(entry.limit) ? entry.limit.context : undefined]
        .find(value => typeof value === 'number' && Number.isSafeInteger(value) && value > 0) as number | undefined
      return [{ id: entry.id, ...(window === undefined ? {} : { contextWindow: window }) }]
    })
  }

  /**
   * Check a saved profile with one plain reply and one tool call.
   * @param raw The profile as shown in the form; it must match the saved one.
   * @returns Whether streaming and tool calls worked, and the reply text.
   */
  async probe(raw: unknown): Promise<{ stream: boolean; toolCall: boolean; text?: string | undefined }> {
    const input = parseSetup(raw)
    const saved = this.settings.get().models.find(model => model.provider === input.provider && model.model === input.model)
    if (saved === undefined || saved.baseURL !== input.baseURL || saved.api !== input.api || saved.contextWindow !== input.contextWindow
      || resolveBudget(saved.contextWindow, saved.maxTokens).outputTokens !== resolveBudget(input.contextWindow, input.maxTokens).outputTokens
      || saved.thinking !== effectiveThinking(input)
      || (input.api === 'openai-completions' && ((saved.thinkingFormat ?? 'openai') !== (input.thinkingFormat ?? 'openai')
        || (saved.maxTokensField ?? 'max_tokens') !== (input.maxTokensField ?? 'max_tokens')))) {
      throw new RpcError('not-saved', localized({ zh: '请先保存当前模型设置，再验证这份已保存的配置。', en: 'Save the current model settings first, then check the saved configuration.' }))
    }
    const resolved = this.resolve({ provider: saved.provider, model: saved.model, thinking: saved.thinking })!
    const reply = await this.complete(resolved, 'Connection diagnostic. Reply only with RainyAgent connected.', [], 180_000)
    if (reply.stopReason === 'error' || reply.stopReason === 'aborted') throw new RpcError('probe-failed', reply.errorMessage ?? localized({ zh: '连接失败。', en: 'The connection failed.' }))
    let text = reply.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('').slice(0, 300)
    let toolCall = false
    const tools = [{ name: 'rainy_probe', description: 'Harmless connection diagnostic; returns the supplied integer.', parameters: Type.Object({ value: Type.Integer() }) }]
    const toolReply = await this.complete(resolved, 'Connection diagnostic. Call rainy_probe with value 7 exactly once. Do not access files or run commands.', tools, 180_000)
    if (toolReply.stopReason === 'error' || toolReply.stopReason === 'aborted') {
      const reason = (toolReply.errorMessage ?? localized({ zh: '未通过', en: 'failed' })).slice(0, 200)
      text += localized({ zh: `\n工具诊断：${reason}`, en: `\nTool check: ${reason}` })
    } else toolCall = toolReply.content.some(block => block.type === 'toolCall' && block.name === 'rainy_probe')
    return { stream: text.length > 0, toolCall, text }
  }

  /**
   * Save a loaded local Strata server as the default model, after this Host reached it itself.
   * @param raw `{ baseURL, maxTokens?, expectedContextWindow? }` from the carrier.
   * @returns The new selection.
   */
  async connectStrata(raw: unknown): Promise<ModelSelection> {
    if (raw === null || typeof raw !== 'object' || !('baseURL' in raw) || typeof raw.baseURL !== 'string') throw new Error(localized({ zh: 'Strata 连接需要本机服务地址。', en: 'Connecting to Strata needs the local service address.' }))
    let health: Awaited<ReturnType<typeof inspectStrataHealth>>
    try { health = await inspectStrataHealth(raw.baseURL) } catch (error) {
      const detail = error instanceof Error ? error.message : localized({ zh: '本地服务未就绪。', en: 'The local service is not ready.' })
      throw new Error(localized({
        zh: `当前执行环境无法连接已加载的 Strata：${detail} Windows 本地模型请使用 Windows 执行环境；不会切换到云端。`,
        en: `This execution environment cannot reach the loaded Strata model: ${detail} Use the Windows execution environment for local Windows models; RainyAgent does not switch to a cloud model.`,
      }))
    }
    if ('expectedContextWindow' in raw && raw.expectedContextWindow !== health.contextWindow) throw new Error(localized({ zh: 'Strata 的实际上下文已改变，请刷新后重新连接。', en: 'The Strata context length changed. Refresh and connect again.' }))
    if ('maxTokens' in raw && (typeof raw.maxTokens !== 'number' || !('expectedContextWindow' in raw))) throw new Error(localized({ zh: '调整输出上限需要确认当前 Strata 上下文。', en: 'Changing the output limit requires confirming the current Strata context length.' }))
    const previous = this.settings.get().models.find(model => model.provider === 'rainy-strata')
    const maxTokens = 'maxTokens' in raw ? raw.maxTokens as number : previous?.maxTokens
    return this.configure({
      provider: 'rainy-strata', baseURL: health.baseURL, model: health.model, contextWindow: health.contextWindow, local: true,
      api: 'openai-completions', thinkingFormat: 'openai', maxTokensField: 'max_tokens', thinking: previous?.thinking ?? 'off',
      ...(maxTokens === undefined ? {} : { maxTokens }),
    })
  }

  /**
   * One non-interactive request (probe, compaction, memory, titles).
   * @param resolved Model.
   * @param prompt User text.
   * @param tools Tool declarations offered to the model.
   * @param timeoutMs Abort after this long.
   * @param options System prompt, output cap, and an outer cancellation signal.
   * @returns The final assistant message; failures are encoded in `stopReason`.
   */
  async complete(resolved: ResolvedModel, prompt: string, tools: { name: string; description: string; parameters: ReturnType<typeof Type.Object> }[], timeoutMs: number,
    options: { system?: string | undefined; maxTokens?: number | undefined; signal?: AbortSignal | undefined; thinking?: ThinkingLevel | undefined } = {}): Promise<AssistantMessage> {
    const system = createInitialSystemMessage(options.system, tools)
    const context = { messages: [...(system === undefined ? [] : [system]), { role: 'user' as const, content: prompt, timestamp: Date.now() }] }
    const signal = options.signal === undefined ? AbortSignal.timeout(timeoutMs) : AbortSignal.any([options.signal, AbortSignal.timeout(timeoutMs)])
    const level = options.thinking ?? resolved.thinking
    const stream = this.streamFn(resolved.model, context as never, {
      signal, ...(options.maxTokens === undefined ? {} : { maxTokens: options.maxTokens }),
      ...(reasoningOption(level) === undefined ? {} : { reasoning: reasoningOption(level) }),
    })
    return stream.result()
  }

  private publish(): void {
    const status = this.status()
    for (const listener of this.listeners) listener(status)
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function credentialShared(left: string, right: string): boolean {
  const claude = new Set(['rainy-claude', 'rainy-claude-haiku'])
  return claude.has(left) && claude.has(right)
}
