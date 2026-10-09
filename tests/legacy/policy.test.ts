import { randomUUID } from 'node:crypto'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import type { GenerateOptions, LlmResolvedModelInfo, StreamChunk } from '@deepseek-ai/dsh-llm'
import { SessionId } from '@deepseek-ai/dsh-session'
import SandboxPolicy from '@deepseek-ai/dsh-sandbox-policy'
import { describe, expect, it, vi } from 'vitest'
import {
  mountAgentLoopTestDependencies,
  mountAgentLoopTestHarness,
} from '../../../packages/test-support/agent-loop-testkit/src/index.ts'
import { MockAdapter, textResponse } from '../../../packages/core/agent-loop/tests/mock-adapter.ts'
import { liveConfig } from '../../../packages/settings/settings/tests/live-config.ts'
import { estimateText } from '../src/budget.ts'
import * as Policy from '../src/policy.ts'

class WindowAdapter extends MockAdapter {
  override resolveModel(provider: string, model: string): Promise<LlmResolvedModelInfo> {
    return Promise.resolve({
      provider,
      id: model,
      name: model,
      context: { contextWindow: model === 'small' ? 4096 : 32768 },
      defaultMaxTokens: 512,
    })
  }
}

/** Holds every stream open until released, recording the provider of each started stream. */
class HeldAdapter extends WindowAdapter {
  readonly started: string[] = []
  private readonly held: (() => void)[] = []
  private open = false
  constructor() {
    super([])
  }
  override async * stream(options: GenerateOptions): AsyncIterable<StreamChunk> {
    this.started.push(options.provider)
    if (!this.open) await new Promise<void>(resolve => this.held.push(resolve))
    yield* textResponse('Done.')
  }
  releaseAll(): void {
    this.open = true
    for (const resume of this.held.splice(0)) resume()
  }
}

describe('Rainy request admission', () => {
  it('rechecks selected extension costs when the active model changes to a smaller window', async ({
    onTestFinished,
  }) => {
    const ctx = new Context()
    onTestFinished(() => ctx.fiber.dispose())
    await mountAgentLoopTestDependencies(ctx)
    await ctx.plugin(SandboxPolicy)
    ctx.provide('compaction', {} as never)
    ctx.provide('tokenMeter', {} as never)
    ctx.provide('spillStore', {} as never)
    ctx.provide('configEditor', { entries: () => [] } as never)
    ctx.provide('rainyProjectRoots', { forSessionCwd: () => [] } as never)
    ctx.provide('workspaceRegistry', {} as never)
    ctx.provide('agentDefaultModel', {} as never)
    await ctx.plugin(Policy, {})
    const adapter = new WindowAdapter([textResponse('Accepted.')])
    ctx.llm.registerAdapter(['mock'], adapter)
    const sessionId = SessionId('extension-window-fixture')
    const description = 'Selected skill detail. '.repeat(100)
    ctx.rainy.extensionDescriptions.set(sessionId, [description])
    ctx.rainy.extensionBudgets.set(sessionId, { maxTokens: 4096, inputRatio: 0.2 })
    const request = {
      provider: 'mock',
      system: description,
      sessionId,
      maxTokens: 512,
      messages: [createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Continue.' }] })],
    }
    const collect = async (model: string) => {
      const chunks = []
      for await (const chunk of ctx.llm.stream({ ...request, model })) chunks.push(chunk)
      return chunks
    }
    expect((await collect('large')).at(-1)).toMatchObject({ type: 'finish', reason: { kind: 'stop' } })
    expect((await collect('small')).at(-1)).toMatchObject({
      type: 'finish',
      reason: { kind: 'error', failure: { code: 'EXTENSION_BUDGET_EXCEEDED' } },
    })
    expect(adapter.requests).toHaveLength(1)
  })

  it('names a removed additional project directory instead of failing with a raw file-system error', async ({
    onTestFinished,
  }) => {
    const ctx = new Context()
    onTestFinished(() => ctx.fiber.dispose())
    await mountAgentLoopTestDependencies(ctx)
    await ctx.plugin(SandboxPolicy)
    const removed = join(tmpdir(), `rainy-removed-root-${randomUUID()}`)
    ctx.provide('compaction', {} as never)
    ctx.provide('tokenMeter', {} as never)
    ctx.provide('spillStore', {} as never)
    ctx.provide('configEditor', { entries: () => [] } as never)
    ctx.provide('rainyProjectRoots', { forSessionCwd: () => [{ path: removed, primary: false }] } as never)
    ctx.provide('workspaceRegistry', {} as never)
    ctx.provide('agentDefaultModel', {} as never)
    await ctx.plugin(Policy, {})
    const session = { id: SessionId('removed-root-fixture'), header: { cwd: tmpdir() } }
    expect(() => ctx.sandboxPolicy.resolve({ session: session as never, mode: 'workspace-write' }))
      .toThrow(`已附加的项目目录不存在：${removed}`)
  })

  it('admits one request per loopback server and parallel requests per remote endpoint unless configured', async ({
    onTestFinished,
  }) => {
    const ctx = new Context()
    onTestFinished(() => ctx.fiber.dispose())
    await mountAgentLoopTestDependencies(ctx)
    await ctx.plugin(SandboxPolicy)
    ctx.provide('compaction', {} as never)
    ctx.provide('tokenMeter', {} as never)
    ctx.provide('spillStore', {} as never)
    const providers = {
      cloud: { baseURL: 'https://api.example.test/v1' },
      ollama: { baseURL: 'http://localhost:11434/v1' },
      windows: { baseURL: 'http://127.0.0.1:8080/v1' },
      wsl: { baseURL: 'http://172.20.0.1:8080/v1' },
      mirror: { baseURL: 'https://mirror.example.test/v1' },
      unset: {},
      constructor: {},
    }
    ctx.provide('configEditor', { entries: () => [{ options: { id: 'llm-pi-ai', config: { providers } } }] } as never)
    ctx.provide('rainyProjectRoots', { forSessionCwd: () => [] } as never)
    ctx.provide('workspaceRegistry', {} as never)
    ctx.provide('agentDefaultModel', {} as never)
    await ctx.plugin(Policy, {
      endpointGroups: { windows: 'strata', wsl: 'strata' },
      endpointConcurrency: { 'https://mirror.example.test': 2 },
    })
    const adapter = new HeldAdapter()
    ctx.llm.registerAdapter(Object.keys(providers), adapter)
    const run = async (provider: string) => {
      const chunks = []
      const messages = [createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Continue.' }] })]
      for await (const chunk of ctx.llm.stream({ provider, model: 'large', maxTokens: 512, messages })) chunks.push(chunk)
      return chunks.at(-1)
    }
    const requested = { cloud: 5, ollama: 2, windows: 1, wsl: 1, mirror: 3, unset: 2, constructor: 2 }
    const runs = Object.entries(requested).flatMap(([provider, count]) => Array.from({ length: count }, () => run(provider)))
    const admitted = { cloud: 4, ollama: 1, strata: 1, mirror: 2, unset: 2, constructor: 2 }
    const total = Object.values(admitted).reduce((sum, count) => sum + count, 0)
    await vi.waitFor(() => {
      expect(adapter.started).toHaveLength(total)
    }, { timeout: 10000 })
    await new Promise(resolve => setTimeout(resolve, 50))
    const started = (...ids: string[]) => adapter.started.filter(id => ids.includes(id)).length
    expect({
      cloud: started('cloud'),
      ollama: started('ollama'),
      strata: started('windows', 'wsl'),
      mirror: started('mirror'),
      unset: started('unset'),
      constructor: started('constructor'),
    }).toEqual(admitted)
    adapter.releaseAll()
    for (const last of await Promise.all(runs)) expect(last).toMatchObject({ type: 'finish', reason: { kind: 'stop' } })
    expect(adapter.started).toHaveLength(runs.length)
  })

  it('defaults to one loopback and four remote requests and rejects non-positive or fractional limits', () => {
    expect(Policy.Config()).toMatchObject({ localEndpointConcurrency: 1, remoteEndpointConcurrency: 4, globalPromptMaxChars: 4000 })
    const invalidLimits = [
      { remoteEndpointConcurrency: 0 }, { localEndpointConcurrency: 1.5 }, { endpointConcurrency: { strata: 0 } },
      { globalPromptMaxChars: 0 }, { globalPromptMaxChars: 1.5 }, { globalPromptMaxChars: 10001 },
    ]
    for (const invalid of invalidLimits) expect(() => Policy.Config(Object.assign(Policy.Config(), invalid))).toThrow()
  })
})

const followup = (text: string) => createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text }] })
const systemText = (request: GenerateOptions | undefined): string => (request?.messages ?? [])
  .filter(message => message.role === 'system')
  .flatMap(message => message.content)
  .map(block => (block.type === 'text' ? block.text : ''))
  .join('\n')

/** Policy behind a real Loader entry; the editor stand-in applies edits the way profile reconciliation does. */
async function globalPromptHarness(ctx: Context, adapter: MockAdapter) {
  await mountAgentLoopTestDependencies(ctx, { systemPrompt: { includeHarnessIdentity: false, includeRuntimeContext: false } })
  await ctx.plugin(SandboxPolicy)
  ctx.provide('compaction', { reduce: async () => null, compactIfNeeded: async () => null } as never)
  ctx.provide('tokenMeter', {} as never)
  ctx.provide('spillStore', {} as never)
  ctx.provide('rainyProjectRoots', { forSessionCwd: () => [] } as never)
  ctx.provide('workspaceRegistry', {} as never)
  ctx.provide('agentDefaultModel', {} as never)
  const edits: Record<string, unknown>[] = []
  const loader: { live?: Awaited<ReturnType<typeof liveConfig>> } = {}
  const edit = async (entry: { options: { config?: object } }, change: (current: Record<string, unknown>) => Record<string, unknown>) => {
    const next = change({ ...entry.options.config })
    edits.push(next)
    await loader.live?.replace(next)
  }
  ctx.provide('configEditor', { entries: () => [], edit } as never)
  loader.live = await liveConfig(ctx, Policy, {})
  ctx.llm.registerAdapter(['mock'], adapter)
  return { edits, loop: await mountAgentLoopTestHarness(ctx) }
}

describe('Rainy global prompt', () => {
  const prompt = 'Answer in Chinese and list verified commands.'

  it('reaches later turns of every session through the logged system prompt and counts as instructions', async ({ onTestFinished }) => {
    const ctx = new Context()
    onTestFinished(() => ctx.fiber.dispose())
    const adapter = new WindowAdapter(Array.from({ length: 4 }, () => textResponse('Done.')))
    const { edits, loop } = await globalPromptHarness(ctx, adapter)
    const state = ctx.rainy
    expect(state.globalPrompt()).toEqual({ text: '', maxChars: 4000 })
    const first = await loop.create(SessionId('global-first'), { provider: 'mock', model: 'large' }, { cwd: tmpdir() })
    first.followup(followup('Before.'))
    await first.whenIdle()
    expect(systemText(adapter.requests[0])).toBe('')

    await expect(state.saveGlobalPrompt({ text: `  ${prompt}\n` })).resolves.toEqual({ text: prompt, maxChars: 4000 })
    expect(edits).toEqual([{ globalPrompt: prompt }])
    expect(ctx.rainy).toBe(state)
    expect(state.globalPrompt().text).toBe(prompt)
    first.followup(followup('After.'))
    await first.whenIdle()
    const second = await loop.create(SessionId('global-second'), { provider: 'mock', model: 'large' }, { cwd: tmpdir() })
    second.followup(followup('New session.'))
    await second.whenIdle()
    expect(adapter.requests.slice(1).map(systemText)).toEqual([prompt, prompt])
    for (const agent of [first, second]) expect(JSON.stringify(agent.session.snapshotEvents())).toContain(prompt)
    expect(state.budgets.get('global-second')?.breakdown?.instructions).toBe(estimateText(prompt))

    await state.saveGlobalPrompt({ text: '' })
    expect(edits.at(-1)).toEqual({})
    second.followup(followup('Cleared.'))
    await second.whenIdle()
    expect(systemText(adapter.requests[3])).toBe('')
    expect(state.budgets.get('global-second')?.breakdown?.instructions).toBe(0)
  })

  it('includes the prompt in request admission for a small model window', async ({ onTestFinished }) => {
    const ctx = new Context()
    onTestFinished(() => ctx.fiber.dispose())
    const adapter = new WindowAdapter([textResponse('Unused.')])
    const { loop } = await globalPromptHarness(ctx, adapter)
    const long = '请先阅读相关文件再修改。'.repeat(100)
    await ctx.rainy.saveGlobalPrompt({ text: long })
    const agent = await loop.create(SessionId('global-small'), { provider: 'mock', model: 'small' }, { cwd: tmpdir() })
    agent.followup(followup('Continue.'))
    await agent.whenIdle()
    expect(adapter.requests).toHaveLength(0)
    const snapshot = ctx.rainy.budgets.get('global-small')
    expect(snapshot?.tokens).toBeGreaterThan(snapshot?.inputLimit ?? Infinity)
    expect(snapshot?.breakdown?.instructions).toBe(estimateText(long))
    expect(snapshot?.error).toContain('输入预算')
  })

  it('rejects an over-limit prompt before writing and refuses to load an over-limit configuration', async ({ onTestFinished }) => {
    const ctx = new Context()
    onTestFinished(() => ctx.fiber.dispose())
    const { edits } = await globalPromptHarness(ctx, new WindowAdapter([]))
    await expect(ctx.rainy.saveGlobalPrompt({ text: 'x'.repeat(4001) })).rejects.toThrow('4000')
    await expect(ctx.rainy.saveGlobalPrompt({ text: 7 })).rejects.toThrow()
    expect(edits).toEqual([])
    const standalone = new Context()
    onTestFinished(() => standalone.fiber.dispose())
    await mountAgentLoopTestDependencies(standalone)
    await standalone.plugin(SandboxPolicy)
    for (const name of ['compaction', 'tokenMeter', 'spillStore', 'configEditor', 'workspaceRegistry', 'agentDefaultModel'])
      standalone.provide(name, {} as never)
    standalone.provide('rainyProjectRoots', { forSessionCwd: () => [] } as never)
    await expect(standalone.plugin(Policy, { globalPrompt: 'abcd', globalPromptMaxChars: 3 })).rejects.toThrow('3 字符上限')
  })
})
