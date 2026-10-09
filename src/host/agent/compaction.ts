/** Rainy retention policy over DSH's durable, tool-pair-safe compaction transactions. */
import { BasicCompactionEngine } from '@deepseek-ai/dsh-compaction-basic'
import type { BasicCompactionConfig } from '@deepseek-ai/dsh-compaction-basic'
import type { Context } from '@deepseek-ai/cordis'
import { z } from 'zod'
import { buildSummarizationInput, selectCompactableRange } from '@deepseek-ai/dsh-compaction-basic/src/region.ts'
import { buildSummarizationMessages, frameSummary, summarizeWithLlm } from '@deepseek-ai/dsh-compaction-basic/src/summarizer.ts'
import type { SummarizationInput, SummaryResult } from '@deepseek-ai/dsh-compaction-basic/src/summarizer.ts'
import type { Agent } from '@deepseek-ai/dsh-agent'
import { createUserMessage, MessageId } from '@deepseek-ai/dsh-llm'
import { SessionSeq, isAppendSurfaceEvent } from '@deepseek-ai/dsh-session'
import type { Session } from '@deepseek-ai/dsh-session'
import type { ProjectionDefinition } from '@deepseek-ai/dsh-session-projection'
import type { TokenMeasurement } from '@deepseek-ai/dsh-token-meter'
import { toolPairingBalancedAfter, toolPairingBalancedBefore } from '@deepseek-ai/dsh-compaction'
import type { CompactionTrigger, CompactionResult } from '@deepseek-ai/dsh-compaction'
import { estimateRequest, estimateText, resolveBudget } from './budget.ts'
import type { Budget } from './budget.ts'

const retainedInputSchema = z.object({
  awaitingInput: z.boolean(),
  users: z.array(z.object({ seq: z.number().int().nonnegative().transform(SessionSeq), id: z.string().transform(MessageId) })),
})
type RetainedInput = z.infer<typeof retainedInputSchema>
declare module '@deepseek-ai/dsh-session-projection' {
  interface SessionProjectionStateMap {
    rainyRetainedInput: RetainedInput
  }
}
const retainedInputProjection: ProjectionDefinition<'rainyRetainedInput'> = {
  key: 'rainyRetainedInput',
  stateVersion: 1,
  stateSchema: retainedInputSchema,
  init: () => ({ awaitingInput: true, users: [] }),
  apply(state, event) {
    if (event.type === 'turn/start') return { ...state, awaitingInput: true }
    if (event.type !== 'user/message' || event.data.source.kind !== 'user' || !isAppendSurfaceEvent(event)) return state
    const user = { seq: event.seq, id: event.data.id }
    return { awaitingInput: false, users: state.awaitingInput ? [user] : [...state.users, user] }
  },
}

type SurfaceRange = { start: SessionSeq; end: SessionSeq }

/** Add Rainy's checkpoint requirements to both the planned and dispatched summary. */
function summaryInput(input: SummarizationInput, budget: Budget): SummarizationInput {
  return { ...input, directive:
    `You are the compaction engine. Write only a progress checkpoint, aiming for at most ${Math.floor(budget.summaryTokens / 2)} tokens and staying below ${budget.summaryTokens} tokens. `
    + 'Use terse prose, no headings or tools. Direct user messages are authoritative reference and remain verbatim outside this checkpoint. '
    + 'Summarize only verified completed progress, exact file paths and sequence or range progress, errors, unresolved work, and the next action. '
    + 'Do not invent, redefine, or restate user goals or constraints. If a prior checkpoint conflicts with direct user instructions, discard that claim. '
    + 'Merge still-valid progress without copying stale claims. Never claim unverified success.' }
}

/**
 * Reserve the full summary instruction before history reaches the input limit.
 * @param budget Resolved conversation window and output reservations.
 * @returns Conservative request size that starts automatic compaction.
 */
export function compactionThreshold(budget: Budget): number {
  const instructionTokens = estimateRequest({ messages: buildSummarizationMessages(summaryInput({ messages: [] }, budget)) })
    - estimateRequest({ messages: [] })
  const summaryLimit = resolveBudget(budget.contextWindow, budget.summaryTokens).inputLimit
  return Math.min(budget.compactAt, Math.max(0, summaryLimit - instructionTokens))
}

/** Select retention per actual model window while retaining the original durable transaction machinery. */
export default class RainyCompaction extends BasicCompactionEngine {
  static override inject = [...BasicCompactionEngine.inject, 'sessionProjections']

  constructor(ctx: Context, config: BasicCompactionConfig = {}) {
    super(ctx, config)
    ctx.effect(() => ctx.sessionProjections.register(retainedInputProjection))
  }

  private retainedInput(session: Session): RetainedInput['users'] {
    const current = new Set(session.surface.nodes)
    const state = this.ctx.sessionProjections.stateOf(session, 'rainyRetainedInput')
    if (state === undefined) throw new Error('Rainy retained-input projection is unavailable.')
    return state.users.filter(user => current.has(user.seq))
  }

  /** Include retained source instructions as reference without adding them to the replacement span. */
  private summaryWithReference(input: SummarizationInput, budget: Budget, session: Session): SummarizationInput {
    const retainedIds = new Set(this.retainedInput(session).map(user => user.id))
    const selected = new Map(input.messages.map(message => [message.id, message]))
    const messages = session.deriveMessages().flatMap((message) => {
      const original = selected.get(message.id)
      return original === undefined ? retainedIds.has(message.id) ? [message] : [] : [original]
    })
    return summaryInput({ ...input, messages }, budget)
  }

  /** Keep the latest admitted turn's user messages at their existing surface positions. */
  private eligibleRanges(session: Session, measurement: TokenMeasurement, retainTokens: number): SurfaceRange[] {
    const range = selectCompactableRange(session, measurement, retainTokens)
    if (range === null) return []
    const protectedSeqs = new Set(this.retainedInput(session).map(user => user.seq))
    const nodes = measurement.nodes
    const ranges: SurfaceRange[] = []
    let start = nodes.findIndex(node => node.seq === range.start)
    const end = nodes.findIndex(node => node.seq === range.end)
    const append = (last: number): void => {
      while (start <= last && !toolPairingBalancedBefore(session, nodes[start].seq)) start++
      while (last >= start && !toolPairingBalancedAfter(session, nodes[last].seq)) last--
      if (start <= last) ranges.push({ start: nodes[start].seq, end: nodes[last].seq })
    }
    for (let index = start; index <= end; index++) {
      if (!protectedSeqs.has(nodes[index].seq)) continue
      append(index - 1)
      start = index + 1
    }
    append(end)
    return ranges
  }

  protected override selectRange(session: Session, measurement: TokenMeasurement, retainTokens: number): SurfaceRange | null {
    return this.eligibleRanges(session, measurement, retainTokens)[0] ?? null
  }

  override async compactRegion(start: SessionSeq, end: SessionSeq, agent: Agent, signal?: AbortSignal): Promise<CompactionResult> {
    const nodes = agent.session.surface.nodes
    const first = nodes.indexOf(start)
    const last = nodes.indexOf(end)
    const selected = new Set(nodes.slice(first, last + 1))
    if (first >= 0 && last >= first && this.retainedInput(agent.session).some(user => selected.has(user.seq))) {
      throw new Error('压缩范围包含当前轮用户原始指令；请选择仅包含执行进度的范围。')
    }
    return super.compactRegion(start, end, agent, signal)
  }

  override async compactIfNeeded(agent: Agent, trigger: CompactionTrigger, signal: AbortSignal): Promise<CompactionResult | null> {
    // The Rainy request counter is the sole automatic pressure owner; the base listener delegates here too.
    if (trigger === 'pressure') return null
    const route = agent.session.requestHeader()?.config
    if (!route) return null
    return this.reduce(agent, signal, true)
  }

  /**
   * Reduce a balanced prefix whose complete summary fits the routed model.
   * @param agent Conversation owning the source history and durable transaction.
   * @param signal Cancellation shared by planning and summarization.
   * @param overflow Prefer the largest eligible prefix after request rejection.
   * @returns The durable reduction, or null when no fitting prefix can shrink.
   */
  async reduce(agent: Agent, signal: AbortSignal, overflow = false): Promise<CompactionResult | null> {
    const route = agent.session.requestHeader()?.config
    if (!route) return null
    const info = await this.ctx.llm.resolveModelInfo(route.provider, route.model, signal)
    const budget = resolveBudget(info.context?.contextWindow ?? 32768, route.maxTokens)
    const measurement = this.ctx.tokenMeter.measure(agent.session)
    const retainedIds = new Set(this.retainedInput(agent.session).map(user => user.id))
    const required = agent.session.deriveMessages().filter(message => message.role === 'system' || retainedIds.has(message.id))
    if (estimateRequest({ messages: required, tools: agent.session.requestHeader()?.tools }) > budget.inputLimit) {
      throw new Error('当前轮用户原始指令与必需上下文超过模型输入预算；请缩短输入或选择更大窗口，原文未被压缩。')
    }
    const maximums = this.eligibleRanges(agent.session, measurement, 0)
    const preferred = overflow ? null : this.selectRange(agent.session, measurement, budget.keepRecentTokens)
    const nodes = measurement.nodes
    const summaryLimit = resolveBudget(budget.contextWindow, budget.summaryTokens).inputLimit
    const replacementTokens = budget.summaryTokens + this.ctx.tokenMeter.estimateMessage(createUserMessage({
      source: { kind: 'system-prompt' },
      content: frameSummary([]),
    }))
    for (const maximum of maximums) {
      const startIndex = nodes.findIndex(node => node.seq === maximum.start)
      const maximumEnd = nodes.findIndex(node => node.seq === maximum.end)
      const preferredEnd = preferred === null ? -1 : nodes.findIndex(node => node.seq === preferred.end)
      const candidates = [preferredEnd, ...Array.from({ length: maximumEnd - startIndex + 1 }, (_, index) => maximumEnd - index)]
      for (const endIndex of new Set(candidates)) {
        if (endIndex > maximumEnd || endIndex < startIndex) continue
        const end = nodes.at(endIndex)?.seq
        if (end === undefined || !toolPairingBalancedAfter(agent.session, end)) continue
        const selected = nodes.slice(startIndex, endIndex + 1)
        if (selected.reduce((sum, node) => sum + node.tokens, 0) <= replacementTokens) continue
        const selectedInput = buildSummarizationInput(agent.session, selected.map(node => node.seq))
        const input = this.summaryWithReference(selectedInput, budget, agent.session)
        const estimated = estimateRequest({ messages: buildSummarizationMessages(input), tools: input.tools })
        if (estimated > summaryLimit) continue
        return this.compactRegion(maximum.start, end, agent, signal)
      }
    }
    return null
  }

  protected override async summarize(input: SummarizationInput, agent: Agent, signal?: AbortSignal): Promise<SummaryResult> {
    const route = agent.session.requestHeader()?.config
    if (!route) throw new Error('请先选择模型再压缩。')
    const info = await this.ctx.llm.resolveModelInfo(route.provider, route.model, signal)
    const budget = resolveBudget(info.context?.contextWindow ?? 32768, route.maxTokens)
    const complete = this.summaryWithReference(input, budget, agent.session)
    if (estimateRequest({ messages: buildSummarizationMessages(complete), tools: complete.tools })
      > resolveBudget(budget.contextWindow, budget.summaryTokens).inputLimit) {
      throw new Error('摘要与原始用户指令的完整参考输入超过模型预算；原始会话保持不变。')
    }
    const result = await summarizeWithLlm(this.ctx, {
      summarizationProvider: route.provider, summarizationModel: route.model, maxTokens: budget.summaryTokens,
    }, complete, agent, signal)
    const text = result.summary.map(block => block.type === 'text' ? block.text : '').join('\n')
    if (estimateText(text) > budget.summaryTokens) throw new Error('摘要超过预算，原始会话保持可恢复；请缩小压缩范围后重试。')
    return result
  }
}
