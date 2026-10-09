/** Model profiles → pi-ai models, and the stream function the agent loop calls. */
import type {
  AnthropicMessagesCompat, Api, AssistantMessage, AssistantMessageEvent, Model, OpenAICompletionsCompat, ProviderStreams,
  SimpleStreamOptions, ThinkingLevelMap, TranscriptContext,
} from '@earendil-works/pi-ai'
import { lazyStream } from '@earendil-works/pi-ai'
import { anthropicMessagesApi } from '@earendil-works/pi-ai/api/anthropic-messages.lazy'
import { openAICompletionsApi } from '@earendil-works/pi-ai/api/openai-completions.lazy'
import { openAIResponsesApi } from '@earendil-works/pi-ai/api/openai-responses.lazy'
import { ANTHROPIC_MODELS } from '@earendil-works/pi-ai/providers/anthropic.models'
import { RequestQueue, resolveBudget } from '../../shared/budget.ts'
import type { ModelSetup, ThinkingLevel, UsageSummary } from '../../shared/rpc.ts'

const PROTOCOLS: Readonly<Record<string, () => ProviderStreams>> = {
  'openai-completions': openAICompletionsApi,
  'openai-responses': openAIResponsesApi,
  'anthropic-messages': anthropicMessagesApi,
}
const protocolCache = new Map<string, ProviderStreams>()

/** No stream event for this long fails the request. */
export const STREAM_IDLE_TIMEOUT_MS = 300_000
/** Upper bound for one model request. */
export const REQUEST_TIMEOUT_MS = 1_800_000

const CLAUDE_CATALOG: ReadonlyMap<string, Model<Api>> = new Map(
  (Array.isArray(ANTHROPIC_MODELS) ? ANTHROPIC_MODELS : Object.values(ANTHROPIC_MODELS)).map((model: Model<Api>) => [model.id, model]),
)

/**
 * The installed Anthropic catalog entry for a Claude model id. Claude Haiku 5.5 is newer than the installed catalog
 * and is described from Claude Sonnet 5.5 with its own price and without mid-conversation tool changes.
 * @param id Model id.
 * @returns The catalog model, or `undefined` for an id the catalog does not know.
 */
export function claudeCatalogModel(id: string): Model<Api> | undefined {
  const known = CLAUDE_CATALOG.get(id)
  if (known !== undefined) return known
  const sibling = CLAUDE_CATALOG.get('claude-sonnet-5-5')
  if (id !== 'claude-haiku-5-5' || sibling === undefined) return undefined
  return {
    ...sibling, id, name: 'Claude Haiku 5.5',
    cost: { input: 0.1, output: 0.5, cacheRead: 0.01, cacheWrite: 0.125 },
    compat: { ...sibling.compat as AnthropicMessagesCompat, supportsMidConvoToolChanges: false },
  }
}

/**
 * RainyAgent levels a model accepts.
 * @param setup Model profile.
 * @returns Supported levels in escalation order.
 */
export function thinkingLevels(setup: Pick<ModelSetup, 'api' | 'model' | 'thinking'>): ThinkingLevel[] {
  const claude = setup.api === 'anthropic-messages' ? claudeCatalogModel(setup.model) : undefined
  if (claude !== undefined) {
    if (!claude.reasoning) return ['off']
    const map = claude.thinkingLevelMap ?? {}
    return (['off', 'low', 'high', 'max'] as const).filter(level => map[level] !== null)
  }
  return setup.thinking === undefined ? ['off'] : ['off', 'low', 'high', 'max']
}

/**
 * The level a request actually uses. Current Claude models cannot turn thinking off, so `off` becomes `low`.
 * @param setup Model profile.
 * @param requested Level chosen for the chat, or the profile default.
 * @returns The level to send.
 */
export function effectiveThinking(setup: Pick<ModelSetup, 'api' | 'model' | 'thinking'>, requested = setup.thinking): ThinkingLevel | undefined {
  if (requested === undefined) return undefined
  const levels = thinkingLevels(setup)
  if (levels.includes(requested)) return requested
  return requested === 'off' && levels.includes('low') ? 'low' : levels.at(-1)
}

const REASONING_EFFORTS: ThinkingLevelMap = { off: 'none', minimal: null, low: 'low', medium: null, high: 'high', xhigh: null, max: 'max' }

/**
 * Build the pi-ai model for a profile.
 * @param setup Saved model profile.
 * @returns The model pi-ai streams; its `maxTokens` is the profile's output budget.
 */
export function toPiModel(setup: ModelSetup): Model<Api> {
  const outputTokens = resolveBudget(setup.contextWindow, setup.maxTokens).outputTokens
  const api = setup.api ?? (setup.local ? 'openai-completions' : 'openai-responses')
  const claude = api === 'anthropic-messages' ? claudeCatalogModel(setup.model) : undefined
  if (claude !== undefined) {
    return { ...claude, provider: setup.provider, baseUrl: setup.baseURL, contextWindow: setup.contextWindow, maxTokens: outputTokens }
  }
  const compat: OpenAICompletionsCompat | undefined = api === 'openai-completions'
    ? { maxTokensField: setup.maxTokensField ?? 'max_tokens', supportsDeveloperRole: false, ...(setup.thinkingFormat === undefined ? {} : { thinkingFormat: setup.thinkingFormat }) }
    : undefined
  return {
    id: setup.model, name: setup.model, api, provider: setup.provider, baseUrl: setup.baseURL,
    reasoning: setup.thinking !== undefined,
    ...(setup.thinking === undefined ? {} : { thinkingLevelMap: REASONING_EFFORTS }),
    input: ['text'],
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    contextWindow: setup.contextWindow,
    maxTokens: outputTokens,
    ...(compat === undefined ? {} : { compat }),
  } as Model<Api>
}

/**
 * @param message A finished model reply.
 * @returns Its token usage, with the cost when the provider prices it.
 */
export function usageOf(message: AssistantMessage): UsageSummary {
  return {
    input: message.usage.input, output: message.usage.output, cacheRead: message.usage.cacheRead, cacheWrite: message.usage.cacheWrite,
    ...(message.usage.reasoning === undefined ? {} : { reasoning: message.usage.reasoning }),
    ...(message.usage.cost.total > 0 ? { cost: message.usage.cost.total } : {}),
  }
}

/**
 * @param a Usage so far.
 * @param b Usage to add.
 * @returns The sum; optional counts stay absent when neither side has them.
 */
export function addUsage(a: UsageSummary, b: UsageSummary): UsageSummary {
  return {
    input: a.input + b.input, output: a.output + b.output, cacheRead: a.cacheRead + b.cacheRead, cacheWrite: a.cacheWrite + b.cacheWrite,
    ...(a.reasoning === undefined && b.reasoning === undefined ? {} : { reasoning: (a.reasoning ?? 0) + (b.reasoning ?? 0) }),
    ...(a.cost === undefined && b.cost === undefined ? {} : { cost: (a.cost ?? 0) + (b.cost ?? 0) }),
  }
}

/**
 * pi-ai reasoning option for a RainyAgent level.
 * @param level Level after {@link effectiveThinking}.
 * @returns The option value; `off` sends no reasoning parameter.
 */
export function reasoningOption(level: ThinkingLevel | undefined): SimpleStreamOptions['reasoning'] {
  return level === undefined || level === 'off' ? undefined : level
}

/** Dependencies of {@link createStreamFn}. */
export interface StreamDeps {
  apiKey(provider: string): string | undefined
  /** Prompt-cache lifetime for a model; Claude keeps the hour-long cache. */
  cacheRetention(model: Model<Api>): SimpleStreamOptions['cacheRetention']
}

const queue = new RequestQueue()

function isLoopback(url: string): boolean {
  try {
    const host = new URL(url).hostname
    return host === 'localhost' || host === '::1' || host === '[::1]' || host.startsWith('127.')
  } catch (_error) {
    return false
  }
}

function failure(model: Model<Api>, partial: AssistantMessage | undefined, message: string): AssistantMessageEvent {
  const base: AssistantMessage = partial ?? {
    role: 'assistant', content: [], api: model.api, provider: model.provider, model: model.id,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
    stopReason: 'error', timestamp: Date.now(),
  }
  return { type: 'error', reason: 'error', error: { ...base, stopReason: 'error', errorMessage: message } }
}

/**
 * The stream function for pi-agent-core. Requests to one local server run one at a time; other endpoints allow four.
 * A request that receives no stream event for {@link STREAM_IDLE_TIMEOUT_MS} fails.
 * @param deps Credential and cache lookups.
 * @returns `(model, context, options) => AssistantMessageEventStream`.
 */
export function createStreamFn(deps: StreamDeps) {
  return (model: Model<Api>, context: TranscriptContext, options: SimpleStreamOptions = {}) => lazyStream(model, async () => {
    const load = PROTOCOLS[model.api]
    if (load === undefined) throw new Error(`Unsupported API protocol ${model.api}`)
    let api = protocolCache.get(model.api)
    if (api === undefined) { api = load(); protocolCache.set(model.api, api) }
    const origin = new URL(model.baseUrl).origin
    const release = await queue.acquire(origin, isLoopback(model.baseUrl) ? 1 : 4, options.signal)
    const controller = new AbortController()
    const forward = (): void => { controller.abort(options.signal?.reason) }
    options.signal?.addEventListener('abort', forward, { once: true })
    let idle = false
    let timer: NodeJS.Timeout | undefined
    const arm = (): void => {
      clearTimeout(timer)
      timer = setTimeout(() => { idle = true; controller.abort(new Error('stream idle timeout')) }, STREAM_IDLE_TIMEOUT_MS)
    }
    const apiKey = deps.apiKey(model.provider)
    const inner = api.streamSimple(model, context, {
      ...options,
      ...(apiKey === undefined ? {} : { apiKey }),
      signal: controller.signal,
      timeoutMs: REQUEST_TIMEOUT_MS,
      maxRetries: 2,
      maxRetryDelayMs: 10_000,
      cacheRetention: options.cacheRetention ?? deps.cacheRetention(model),
      ...(model.api === 'anthropic-messages'
        ? { onPayload: (payload: unknown, payloadModel: Model<Api>) => { blockContent(payload); markPreviousTurn(payload); return options.onPayload?.(payload, payloadModel) } }
        : {}),
    })
    return (async function* events(): AsyncGenerator<AssistantMessageEvent> {
      let partial: AssistantMessage | undefined
      try {
        arm()
        for await (const event of inner) {
          arm()
          if ('partial' in event) partial = event.partial
          if (event.type === 'error' && idle) {
            yield failure(model, partial, `模型在 ${STREAM_IDLE_TIMEOUT_MS / 1000} 秒内没有返回数据，请求已超时。`)
            return
          }
          yield event
        }
      } finally {
        clearTimeout(timer)
        options.signal?.removeEventListener('abort', forward)
        release()
      }
    })()
  })
}

// ── Anthropic prompt cache ──
// Derived from the DeepSeek Harness pi-ai adapter (MIT, see LICENSE.upstream).

const MAX_BREAKPOINTS = 4
const MARKABLE = new Set(['text', 'image', 'tool_result'])

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function marks(blocks: unknown): number {
  return Array.isArray(blocks) ? blocks.filter(block => isRecord(block) && block.cache_control !== undefined).length : 0
}

/**
 * Send every message's text as content blocks. A breakpoint can only sit on a block, so pi-ai and
 * {@link markPreviousTurn} turn a string into a block to mark it; without this, the same message would change form
 * between requests depending on whether it carries the breakpoint. The payload is changed in place.
 * @param payload Anthropic Messages request body.
 */
export function blockContent(payload: unknown): void {
  if (!isRecord(payload) || !Array.isArray(payload.messages)) return
  for (const message of payload.messages) {
    if (isRecord(message) && typeof message.content === 'string' && message.content !== '') message.content = [{ type: 'text', text: message.content }]
  }
}

/**
 * pi-ai marks the system prompt, the last tool and the last user message. Anthropic looks back only about 20 blocks
 * from a breakpoint, so a step that adds many tool results would write the whole conversation again. Marking the user
 * message that ended the previous request as well keeps that entry an exact hit. The payload is changed in place.
 * @param payload Anthropic Messages request body.
 * @returns `undefined`, so pi-ai sends its own (now marked) object.
 */
export function markPreviousTurn(payload: unknown): undefined {
  if (!isRecord(payload) || !Array.isArray(payload.messages)) return undefined
  const messages = payload.messages.filter(isRecord)
  const used = marks(payload.system) + marks(payload.tools) + messages.reduce((total, message) => total + marks(message.content), 0)
  if (used === 0 || used >= MAX_BREAKPOINTS) return undefined
  const before = (from: number, role: string): number => {
    let index = from
    while (index >= 0 && messages[index]?.role !== role) index--
    return index
  }
  const previous = before(before(before(messages.length - 1, 'user') - 1, 'assistant') - 1, 'user')
  const message = messages[previous]
  if (message === undefined) return undefined
  let cacheControl: unknown = { type: 'ephemeral' }
  for (const item of messages) {
    const block = Array.isArray(item.content) ? item.content.find(entry => isRecord(entry) && entry.cache_control !== undefined) : undefined
    if (isRecord(block)) { cacheControl = block.cache_control; break }
  }
  if (typeof message.content === 'string') {
    message.content = [{ type: 'text', text: message.content, cache_control: cacheControl }]
    return undefined
  }
  if (!Array.isArray(message.content) || marks(message.content) > 0) return undefined
  const last: unknown = message.content.at(-1)
  if (isRecord(last) && typeof last.type === 'string' && MARKABLE.has(last.type)) last.cache_control = cacheControl
  return undefined
}

/**
 * Remove the breakpoint pi-ai puts on the last user message of an Anthropic request, after {@link markPreviousTurn}
 * ran. A compaction request reads the previous request's cache entry through the breakpoint on that request's last user
 * message; writing its own tail would cost a cache write nothing reads, since the history changes when the checkpoint
 * lands. Messages after the last user message, such as pi-ai's per-message effort, carry no breakpoint. The payload
 * is changed in place.
 * @param payload Anthropic Messages request body.
 * @returns `undefined`, so pi-ai sends its own object.
 */
export function unmarkLastUserMessage(payload: unknown): undefined {
  if (!isRecord(payload) || !Array.isArray(payload.messages)) return undefined
  const last: unknown = payload.messages.findLast(message => isRecord(message) && message.role === 'user')
  if (isRecord(last) && Array.isArray(last.content)) for (const block of last.content) if (isRecord(block)) delete block.cache_control
  return undefined
}
