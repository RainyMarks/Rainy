/** Model-specific request budgets and explicit estimated token counts. */
export interface Budget {
  contextWindow: number
  outputTokens: number
  marginTokens: number
  inputLimit: number
  compactAt: number
  keepRecentTokens: number
  summaryTokens: number
  toolTokens: number
}

/** Resolve a combined input/output window; explicit output limits are never silently lowered. */
export function resolveBudget(contextWindow: number, maxOutput?: number): Budget {
  if (!Number.isSafeInteger(contextWindow) || contextWindow < 4096)
    throw new Error('上下文长度必须是至少 4096 的整数。')
  const outputTokens = maxOutput ?? Math.min(16000, Math.floor(contextWindow * 0.16))
  if (!Number.isSafeInteger(outputTokens) || outputTokens < 1) throw new Error('输出上限必须是正整数。')
  const marginTokens = Math.max(512, Math.floor(contextWindow * 0.08))
  const inputLimit = contextWindow - outputTokens - marginTokens
  if (inputLimit < 1024) throw new Error('输出预留过大，当前窗口没有足够的输入空间。')
  return {
    contextWindow,
    outputTokens,
    marginTokens,
    inputLimit,
    compactAt: Math.min(Math.floor(contextWindow * 0.7), inputLimit),
    keepRecentTokens: Math.min(Math.floor(contextWindow * 0.2), Math.floor(inputLimit * 0.4)),
    summaryTokens: Math.min(4000, Math.floor(contextWindow * 0.04)),
    toolTokens: Math.min(4000, Math.floor(contextWindow * 0.04)),
  }
}

/** A counter must include provider framing and tool definitions, not only visible conversation text. */
export interface TokenCounter {
  count(request: CountableRequest, signal?: AbortSignal): Promise<TokenCount>
}
/** Request fields that contribute tokens; ids, secrets and transport options are excluded. */
export interface CountableRequest {
  provider?: string
  model?: string
  system?: string
  messages: readonly { id?: string; role: string; content: readonly unknown[]; source?: { kind: string } }[]
  tools?: readonly unknown[]
}
/** Exactness is carried into the UI; estimated counts never masquerade as tokenizer measurements. */
export interface TokenCount {
  tokens: number
  kind: 'estimated' | 'exact'
  method: string
}

/** Conservative component estimates; provider-reported totals may differ from this decomposition. */
export interface PromptBreakdown {
  system: number
  tools: number
  instructions: number
  memory: number
  extensions: number
  history: number
  framing: number
}

/**
 * Explain prompt cost without retaining prompt text in the settings API.
 * @param request Final model-facing request.
 * @param extensionDescriptions Selected Skills and MCP instructions to attribute separately from ordinary system text.
 * @param memoryMessageIds Recall ids established by the durable project-memory projection.
 * @param systemInstructions User-authored system-prompt texts, such as the global prompt, attributed to instructions.
 * @returns Estimated tokens by content source, with separate protocol framing.
 */
export function promptBreakdown(
  request: CountableRequest,
  extensionDescriptions: readonly string[] = [],
  memoryMessageIds: readonly string[] = [],
  systemInstructions: readonly string[] = [],
): PromptBreakdown {
  const counts: PromptBreakdown = {
    system: request.system ? estimateText(request.system) : 0,
    tools: request.tools ? tokens(serializedCost(request.tools)) : 0,
    instructions: 0,
    memory: 0,
    extensions: 0,
    history: 0,
    framing: 16 * (request.messages.length + 1),
  }
  const systemTexts = [request.system ?? '']
  for (const message of request.messages) {
    const count = tokens(serializedCost(message.content))
    if (message.role === 'system') {
      counts.system += count
      for (const block of message.content) {
        if (block !== null && typeof block === 'object' && 'text' in block && typeof block.text === 'string')
          systemTexts.push(block.text)
      }
    } else if (message.source?.kind === 'agent-instructions' || message.source?.kind === 'system-prompt')
      counts.instructions += count
    else if (message.id !== undefined && memoryMessageIds.includes(message.id)) counts.memory += count
    else counts.history += count
  }
  let remaining = systemTexts.join('\n')
  const attribute = (texts: readonly string[]): number => {
    let moved = 0
    for (const text of texts) {
      if (!text || !remaining.includes(text)) continue
      moved += estimateText(text)
      remaining = remaining.replace(text, '')
    }
    return moved
  }
  counts.extensions = attribute(extensionDescriptions)
  const instructions = attribute(systemInstructions)
  counts.instructions += instructions
  counts.system = Math.max(0, counts.system - counts.extensions - instructions)
  return counts
}

/** ASCII character count and UTF-8 byte count of all other characters in one text. */
interface TextCost {
  ascii: number
  other: number
}

function textCost(text: string): TextCost {
  let ascii = 0
  let other = 0
  for (let index = 0; index < text.length; index++) {
    const unit = text.charCodeAt(index)
    if (unit < 0x80) ascii++
    else if (unit < 0x800) other += 2
    else if (unit >= 0xd800 && unit <= 0xdbff && (text.charCodeAt(index + 1) & 0xfc00) === 0xdc00) {
      // A surrogate pair is one four-byte character; a lone surrogate costs three bytes like other BMP units.
      other += 4
      index++
    } else other += 3
  }
  return { ascii, other }
}

function tokens(cost: TextCost): number {
  return Math.ceil(cost.ascii / 3) + cost.other
}

/** Conservative multilingual estimate. ASCII costs one token per three characters, non-ASCII up to its UTF-8 byte count. */
export function estimateText(text: string): number {
  return tokens(textCost(text))
}

function deeplyFrozen(value: unknown): boolean {
  return value === null || typeof value !== 'object' || (Object.isFrozen(value) && Object.values(value).every(deeplyFrozen))
}

/** Costs of deeply frozen content and tool lists, which cannot change; Session messages qualify, so each is priced once. */
const frozenCosts = new WeakMap<object, TextCost>()

function serializedCost(value: readonly unknown[]): TextCost {
  const cached = frozenCosts.get(value)
  if (cached) return cached
  const cost = textCost(JSON.stringify(value))
  if (deeplyFrozen(value)) frozenCosts.set(value, cost)
  return cost
}

/** Price the complete serialized model-facing data and fixed chat framing. */
export function estimateRequest(request: CountableRequest): number {
  // Prices JSON.stringify({ system, messages: [{ role, content }, …], tools }) by part, plus the commas between messages.
  // JSON output escapes lone surrogates, so no part boundary splits a character.
  const total = textCost(JSON.stringify({ system: request.system, messages: [] }))
  const parts: TextCost[] = request.tools === undefined ? [] : [textCost(',"tools":'), serializedCost(request.tools)]
  for (const message of request.messages)
    parts.push(textCost(`{"role":${JSON.stringify(message.role)},"content":}`), serializedCost(message.content))
  for (const part of parts) {
    total.ascii += part.ascii
    total.other += part.other
  }
  total.ascii += Math.max(0, request.messages.length - 1)
  return tokens(total) + 16 * (request.messages.length + 1)
}

/** Maintain route-local calibration using actual provider usage; calibration only increases the estimate. */
export class CalibratedCounter implements TokenCounter {
  private ratio = 1
  count(request: CountableRequest, signal?: AbortSignal): Promise<TokenCount> {
    signal?.throwIfAborted()
    return Promise.resolve({
      tokens: Math.ceil(estimateRequest(request) * this.ratio),
      kind: 'estimated',
      method: 'multilingual-conservative+usage',
    })
  }
  observe(estimated: number, actualInput: number): void {
    if (estimated > 0 && actualInput > estimated) this.ratio *= (actualInput / estimated) * 1.1
  }
}

/** An optional service-provided whole-request counter, configured explicitly for the deployed chat template. */
export class EndpointCounter implements TokenCounter {
  constructor(
    private readonly url: string,
    private readonly fallback: TokenCounter,
  ) {}
  async count(request: CountableRequest, signal?: AbortSignal): Promise<TokenCount> {
    try {
      const response = await fetch(this.url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          provider: request.provider,
          model: request.model,
          system: request.system,
          messages: request.messages.map(message => ({ role: message.role, content: message.content })),
          tools: request.tools,
        }),
        signal: AbortSignal.any([...(signal ? [signal] : []), AbortSignal.timeout(5000)]),
      })
      if (!response.ok) throw new Error(`Token counter HTTP ${response.status}`)
      const data: unknown = await response.json()
      if (
        data === null ||
        typeof data !== 'object' ||
        !('tokens' in data) ||
        !Number.isSafeInteger(data.tokens) ||
        Number(data.tokens) < 0
      )
        throw new Error('Invalid token counter response')
      if (
        request.model !== undefined &&
        (!('model' in data) ||
          data.model !== request.model ||
          !('chatTemplate' in data) ||
          typeof data.chatTemplate !== 'string' ||
          !data.chatTemplate.trim())
      )
        return await this.fallback.count(request, signal)
      return { tokens: Number(data.tokens), kind: 'exact', method: 'configured-request-tokenizer' }
    } catch (error) {
      if (signal?.aborted) throw error
      return this.fallback.count(request, signal)
    }
  }
}

interface Lane {
  active: number
  waiting: { limit: number; admit: () => void }[]
}

/** FIFO, cancellation-aware request admission with a concurrency limit per endpoint key. */
export class RequestQueue {
  private readonly lanes = new Map<string, Lane>()

  /**
   * Wait until the key admits this request after every earlier waiter for the same key.
   * @param key Queue shared by the requests to one model server.
   * @param limit Concurrent admissions for the key; the oldest waiter's limit governs each admission.
   * @param signal Cancels only this wait; a cancelled waiter neither holds nor frees a slot.
   * @returns A release for the admitted slot; repeated calls are ignored.
   */
  async acquire(key: string, limit: number, signal?: AbortSignal): Promise<() => void> {
    signal?.throwIfAborted()
    const lane = this.lanes.get(key) ?? { active: 0, waiting: [] }
    this.lanes.set(key, lane)
    let cancel: (() => void) | undefined
    try {
      await new Promise<void>((resolve, reject) => {
        const waiter = { limit, admit: resolve }
        cancel = () => {
          const index = lane.waiting.indexOf(waiter)
          if (index < 0) return
          lane.waiting.splice(index, 1)
          this.advance(key, lane)
          const reason: unknown = signal?.reason
          reject(reason instanceof Error ? reason : new Error('Cancelled'))
        }
        signal?.addEventListener('abort', cancel, { once: true })
        lane.waiting.push(waiter)
        this.advance(key, lane)
      })
    } finally {
      if (cancel) signal?.removeEventListener('abort', cancel)
    }
    let released = false
    const release = () => {
      if (released) return
      released = true
      lane.active--
      this.advance(key, lane)
    }
    if (signal?.aborted) {
      release()
      signal.throwIfAborted()
    }
    return release
  }

  private advance(key: string, lane: Lane): void {
    while (lane.waiting.length > 0 && lane.active < lane.waiting[0].limit) {
      lane.active++
      lane.waiting.shift()?.admit()
    }
    if (lane.active === 0 && lane.waiting.length === 0) this.lanes.delete(key)
  }
}
