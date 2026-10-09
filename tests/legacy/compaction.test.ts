import { Context } from '@deepseek-ai/cordis'
import type { Agent } from '@deepseek-ai/dsh-agent'
import LlmRuntime, { createMessage, createSystemMessage, createToolResultMessage, createUserMessage, ToolCallId } from '@deepseek-ai/dsh-llm'
import type { LlmResolvedModelInfo } from '@deepseek-ai/dsh-llm'
import SessionStore, { Session, SessionId } from '@deepseek-ai/dsh-session'
import SessionProjectionRegistry from '@deepseek-ai/dsh-session-projection'
import TokenMeter from '@deepseek-ai/dsh-token-meter'
import { selectCompactableRange } from '@deepseek-ai/dsh-compaction-basic/src/region.ts'
import { describe, expect, it, vi } from 'vitest'
import { MockAdapter, textResponse } from '../../../packages/core/agent-loop/tests/mock-adapter.ts'
import RainyCompaction, { compactionThreshold } from '../src/compaction.ts'
import { estimateRequest, resolveBudget } from '../src/budget.ts'

class SmallWindowAdapter extends MockAdapter {
  override resolveModel(provider: string, model: string): Promise<LlmResolvedModelInfo> {
    return Promise.resolve({ provider, id: model, name: model, context: { contextWindow: 4096 } })
  }
}

function sessionWithReads(): Session {
  const session = Session.create(SessionId('small-window-compaction'))
  session.append('turn/start', { turn: 1 })
  session.append('system/message', {
    turn: 1, step: 1, message: createSystemMessage('Required system instruction. '.repeat(55)),
  }, { surfaceOp: 'append' })
  session.append('user/message', createUserMessage({
    source: { kind: 'user' }, content: [{ type: 'text', text: 'Read the project. Preserve RAINY-CONSTRAINT-714.' }],
  }), { surfaceOp: 'append' })
  session.append('request/header', { reason: 'initial', header: {
    config: { provider: 'mock', model: 'small', maxTokens: 655 },
    tools: [{ name: 'read', description: 'Required schema description. '.repeat(120), parameters: { type: 'object', properties: {} } }],
  } })
  appendReads(session, 1, 3)
  return session
}

function appendReads(session: Session, first: number, last: number): void {
  for (let step = first; step <= last; step++) {
    const callId = ToolCallId(`read-${step}`)
    session.append('step/start', { turn: 1, step })
    session.append('assistant/message', { turn: 1, step, stream: [], message: createMessage({
      role: 'assistant', source: { kind: 'model', provider: 'mock', model: 'small' },
      content: [{ type: 'tool-call', id: callId, name: 'read', arguments: '{"file_path":"project/example.txt"}' }],
    }) }, { surfaceOp: 'append' })
    session.append('tool/call', { turn: 1, step, callId, name: 'read', arguments: '{"file_path":"project/example.txt"}' })
    session.append('tool/result', { turn: 1, step, message: createToolResultMessage({
      callId, isError: false, content: [{ type: 'text', text: `Verified file ${step}. ` + 'value '.repeat(75) }],
    }) }, { surfaceOp: 'append' })
    session.append('step/end', { turn: 1, step })
  }
}

function fixture(onTestFinished: (cleanup: () => void | Promise<void>) => void) {
  const ctx = new Context()
  onTestFinished(() => ctx.fiber.dispose())
  new LlmRuntime(ctx)
  new SessionProjectionRegistry(ctx)
  new TokenMeter(ctx)
  const adapter = new SmallWindowAdapter(Array.from({ length: 8 }, () => textResponse('Completed earlier reads. Continue from the latest verified file.')))
  ctx.llm.registerAdapter(['mock'], adapter)
  const engine = new RainyCompaction(ctx, { auto: false })
  return { ctx, adapter, engine }
}

describe('small-window compaction planning', () => {
  it('accounts for the full summary directive before the ordinary input budget is exhausted', () => {
    const budget = resolveBudget(4096)
    expect(compactionThreshold(budget)).toBeLessThanOrEqual(budget.compactAt)
    expect(compactionThreshold(budget)).toBeGreaterThan(0)
  })

  it('fits a shrinking balanced prefix when the default recent tail would retain the entire history', async ({ onTestFinished }) => {
    const ctx = new Context()
    onTestFinished(() => ctx.fiber.dispose())
    new LlmRuntime(ctx)
    new SessionProjectionRegistry(ctx)
    new TokenMeter(ctx)
    const adapter = new SmallWindowAdapter([textResponse('Read project/example.txt; preserve RAINY-CONSTRAINT-714. Continue reading.')])
    ctx.llm.registerAdapter(['mock'], adapter)
    const engine = new RainyCompaction(ctx, { auto: false })
    const session = sessionWithReads()
    const agent = { session, options: { provider: 'mock', model: 'small' } } as Agent
    const budget = resolveBudget(4096)
    expect(selectCompactableRange(session, ctx.tokenMeter.measure(session), budget.keepRecentTokens)).toBeNull()
    const before = estimateRequest({ messages: session.deriveMessages(), tools: session.requestHeader()?.tools })
    const result = await engine.reduce(agent, new AbortController().signal)
    expect(result).not.toBeNull()
    const summary = adapter.requests[0]
    expect(summary.purpose).toBe('compaction')
    expect(estimateRequest(summary)).toBeLessThanOrEqual(resolveBudget(4096, budget.summaryTokens).inputLimit)
    expect(summary.messages.at(-1)?.content).toMatchInlineSnapshot(`
      [
        {
          "text": "You are the compaction engine. Write only a progress checkpoint, aiming for at most 81 tokens and staying below 163 tokens. Use terse prose, no headings or tools. Direct user messages are authoritative reference and remain verbatim outside this checkpoint. Summarize only verified completed progress, exact file paths and sequence or range progress, errors, unresolved work, and the next action. Do not invent, redefine, or restate user goals or constraints. If a prior checkpoint conflicts with direct user instructions, discard that claim. Merge still-valid progress without copying stale claims. Never claim unverified success.",
          "type": "text",
        },
      ]
    `)
    expect(JSON.stringify(summary.messages)).toContain('Required system instruction.')
    expect(summary.tools?.[0]?.description).toContain('Required schema description.')
    const history = session.deriveMessages()
    expect(JSON.stringify(history)).toContain('RAINY-CONSTRAINT-714')
    expect(JSON.stringify(history)).toContain('read-3')
    expect(estimateRequest({ messages: history, tools: session.requestHeader()?.tools })).toBeLessThan(before)
    expect(await engine.reduce(agent, new AbortController().signal)).toBeNull()
    expect(adapter.requests).toHaveLength(1)
  })

  it('retains exact current-turn instructions across repeated summaries and cold replay without copying them', async ({ onTestFinished }) => {
    const { engine, adapter } = fixture(onTestFinished)
    let session = sessionWithReads()
    const original = session.deriveMessages().find(message => message.role === 'user' && message.source.kind === 'user')
    expect(original).toBeDefined()
    const reduce = () => engine.reduce({ session, options: { provider: 'mock', model: 'small' } } as Agent, new AbortController().signal, true)
    expect(await reduce()).not.toBeNull()
    appendReads(session, 4, 7)
    expect(await reduce()).not.toBeNull()
    // The test seed represents a cold process opening the complete durable log.
    session = Session.create(session.id, session.snapshotEvents(), session.header)
    const replayFixture = fixture(onTestFinished)
    appendReads(session, 8, 11)
    expect(await replayFixture.engine.reduce({ session, options: { provider: 'mock', model: 'small' } } as Agent, new AbortController().signal, true)).not.toBeNull()
    expect(session.deriveMessages().filter(message => message.id === original?.id)).toEqual([original])
    expect([...adapter.requests, ...replayFixture.adapter.requests]).toHaveLength(3)
    for (const request of [...adapter.requests, ...replayFixture.adapter.requests]) {
      expect(request.messages.filter(message => message.id === original?.id)).toEqual([original])
      expect(JSON.stringify(request.messages.at(-1)?.content)).toContain('If a prior checkpoint conflicts with direct user instructions, discard that claim.')
      expect(estimateRequest(request)).toBeLessThanOrEqual(resolveBudget(4096, resolveBudget(4096).summaryTokens).inputLimit)
    }
  })

  it('protects every steered input in the admitted turn and switches only after the next user input arrives', async ({ onTestFinished }) => {
    const { engine, ctx } = fixture(onTestFinished)
    const session = sessionWithReads()
    const original = session.deriveMessages().find(message => message.role === 'user' && message.source.kind === 'user')
    const steering = createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Also preserve exact constraint RAINY-STEER-29.' }] })
    session.append('user/message', steering, { surfaceOp: 'append' })
    appendReads(session, 4, 7)
    const agent = { session, options: { provider: 'mock', model: 'small' } } as Agent
    expect(await engine.reduce(agent, new AbortController().signal, true)).not.toBeNull()
    expect(await engine.reduce(agent, new AbortController().signal, true)).not.toBeNull()
    const retained = session.deriveMessages().filter(message => message.id === original?.id || message.id === steering.id)
    expect(retained).toEqual([original, steering])
    session.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
    session.append('turn/start', { turn: 2 })
    expect(ctx.sessionProjections.stateOf(session, 'rainyRetainedInput')?.users.map(user => user.id)).toEqual([original?.id, steering.id])
    const next = createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Start the next task.' }] })
    session.append('user/message', next, { surfaceOp: 'append' })
    expect(ctx.sessionProjections.stateOf(session, 'rainyRetainedInput')?.users.map(user => user.id)).toEqual([next.id])
  })

  it('rejects an explicit range containing current instructions before opening a compaction transaction', async ({ onTestFinished }) => {
    const { engine, ctx, adapter } = fixture(onTestFinished)
    const session = sessionWithReads()
    const range = selectCompactableRange(session, ctx.tokenMeter.measure(session), 0)
    expect(range).not.toBeNull()
    if (!range) throw new Error('Fixture must provide a generic compactable range.')
    const before = session.seq
    await expect(engine.compactRegion(range.start, range.end, { session } as Agent)).rejects.toThrow('当前轮用户原始指令')
    expect(session.seq).toBe(before)
    expect(adapter.requests).toHaveLength(0)
  })

  it('reports oversized protected instructions without summarizing or mutating the session', async ({ onTestFinished }) => {
    const { engine, adapter } = fixture(onTestFinished)
    const session = sessionWithReads()
    session.append('user/message', createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Required exact user text. '.repeat(3000) }] }), { surfaceOp: 'append' })
    const before = session.seq
    await expect(engine.reduce({ session } as Agent, new AbortController().signal)).rejects.toThrow('超过模型输入预算')
    expect(session.seq).toBe(before)
    expect(adapter.requests).toHaveLength(0)
  })

  it('uses retained input as reference for idle manual compaction while preserving its exact surface message', async ({ onTestFinished }) => {
    const { ctx, engine, adapter } = fixture(onTestFinished)
    new SessionStore(ctx)
    const flush = vi.spyOn(ctx.sessions, 'flush').mockResolvedValue(false)
    const session = sessionWithReads()
    session.append('turn/end', { turn: 1, reason: { kind: 'completed' } })
    const original = session.deriveMessages().find(message => message.role === 'user' && message.source.kind === 'user')
    const signal = new AbortController().signal
    const agent = { session, options: { provider: 'mock', model: 'small' },
      runMaintenance: <T>(task: (maintenanceSignal: AbortSignal) => Promise<T>): Promise<T> => task(signal),
    } as Agent
    const result = await engine.compactNow(agent, signal)
    expect(result).not.toBeNull()
    expect(flush).toHaveBeenCalledOnce()
    expect(session.deriveMessages().filter(message => message.id === original?.id)).toEqual([original])
    expect(adapter.requests[0].messages.filter(message => message.id === original?.id)).toEqual([original])
  })

  it('rejects an oversized complete auxiliary reference before dispatch and leaves all source messages visible', async ({ onTestFinished }) => {
    const { engine, adapter } = fixture(onTestFinished)
    const session = sessionWithReads()
    session.append('user/message', createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Required exact text. '.repeat(3000) }] }), { surfaceOp: 'append' })
    const before = session.deriveMessages()
    const start = session.surface.nodes.at(2)
    const end = session.surface.nodes.at(-2)
    if (start === undefined || end === undefined) throw new Error('Fixture must include complete read progress.')
    await expect(engine.compactRegion(start, end, { session, options: { provider: 'mock', model: 'small' } } as Agent))
      .rejects.toThrow('完整参考输入超过模型预算')
    expect(session.deriveMessages()).toEqual(before)
    expect(adapter.requests).toHaveLength(0)
  })
})
