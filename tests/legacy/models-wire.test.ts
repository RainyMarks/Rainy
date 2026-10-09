import { Context } from '@deepseek-ai/cordis'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { PiAiAdapter } from '@deepseek-ai/dsh-llm-pi-ai'
import type { PiAiProviderProfile } from '@deepseek-ai/dsh-llm-pi-ai'
import { afterEach, expect, it, onTestFinished } from 'vitest'
import { resolveProfiles } from '../../../packages/llm/llm-pi-ai/src/config.ts'
import { memoryAuth } from '../../../packages/llm/llm-pi-ai/tests/auth-double.ts'
import { closeMockServers, mockServer, textEvents } from '../../../packages/llm/llm-pi-ai/tests/mock-server.ts'
import { CLAUDE_HAIKU, CLAUDE_OPUS, configureModel } from '../src/models.ts'
import type { ModelSetup } from '../src/models.ts'

afterEach(closeMockServers)

it.each([
  { api: 'openai-completions', thinkingFormat: 'deepseek', thinking: 'off', expected: { thinking: { type: 'disabled' } } },
  { api: 'openai-responses', thinkingFormat: 'deepseek', thinking: 'off', expected: { reasoning: { effort: 'none' } } },
  { api: 'openai-completions', thinkingFormat: 'qwen', thinking: 'off', expected: { enable_thinking: false } },
  { api: 'openai-completions', thinkingFormat: 'deepseek', thinking: undefined, expected: undefined },
] as const)('preserves the saved $thinking selection using $api / $thinkingFormat', async ({ api, thinkingFormat, thinking, expected }) => {
  const events = api === 'openai-responses'
    ? [
      JSON.stringify({ type: 'response.created', response: { id: 'response-off' } }),
      JSON.stringify({ type: 'response.output_item.added', output_index: 0, item: { type: 'message', id: 'message-off', role: 'assistant', content: [] } }),
      JSON.stringify({ type: 'response.content_part.added', output_index: 0, content_index: 0, part: { type: 'output_text', text: '' } }),
      JSON.stringify({ type: 'response.output_text.delta', output_index: 0, content_index: 0, delta: 'hello' }),
      JSON.stringify({ type: 'response.output_item.done', output_index: 0, item: { type: 'message', id: 'message-off', role: 'assistant', content: [{ type: 'output_text', text: 'hello' }] } }),
      JSON.stringify({ type: 'response.completed', response: { id: 'response-off', status: 'completed', output: [],
        usage: { input_tokens: 3, output_tokens: 1, total_tokens: 4 } } }),
    ]
    : textEvents
  const server = await mockServer([{ events }])
  const ctx = new Context()
  onTestFinished(() => ctx.fiber.dispose())
  let providers: Record<string, PiAiProviderProfile> = {}
  ctx.provide('credentials', { describe: async () => ({ configured: true }) } as never)
  ctx.provide('agentDefaultModel', { saveSelection: async () => undefined } as never)
  ctx.provide('configEditor', {
    entries: () => [{ options: { id: 'llm-pi-ai' } }],
    edit: async (
      _entry: object,
      update: (current: { providers: Record<string, PiAiProviderProfile> }) => { providers: Record<string, PiAiProviderProfile> },
    ) => {
      providers = update({ providers }).providers
    },
  } as never)
  const setup: ModelSetup = { provider: 'rainy-wire', model: 'declared-model', baseURL: server.url, api, local: false,
    contextWindow: 4096, maxTokens: 163, thinking, thinkingFormat }
  await configureModel(ctx, setup)
  const adapter = new PiAiAdapter({ profiles: () => resolveProfiles(providers), resolveApiKey: () => Promise.resolve('test-key'), auth: memoryAuth() })
  const chunks = []
  for await (const chunk of adapter.stream({ provider: setup.provider, model: setup.model, maxTokens: 163,
    messages: [createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Return a concise checkpoint.' }] })] })) chunks.push(chunk)
  expect(chunks.at(-1)).toMatchObject({ type: 'finish', reason: { kind: 'stop' } })
  expect(server.requests).toHaveLength(1)
  if (expected === undefined) {
    for (const field of ['thinking', 'reasoning', 'reasoning_effort', 'enable_thinking']) expect(server.requests[0]).not.toHaveProperty(field)
  } else expect(server.requests[0]).toMatchObject(expected)
})

it('saves a relayed Claude model with its own efforts, low effort for "off" and an hour-long cache', async () => {
  const sse = (type: string, data: Record<string, unknown>): string => `${JSON.stringify({ type, ...data })}\nevent: ${type}`
  const server = await mockServer([{ events: [
    sse('message_start', { message: { id: 'msg_1', type: 'message', role: 'assistant', model: 'claude-opus-5-5', content: [],
      stop_reason: null, usage: { input_tokens: 3, output_tokens: 1 } } }),
    sse('content_block_start', { index: 0, content_block: { type: 'text', text: '' } }),
    sse('content_block_delta', { index: 0, delta: { type: 'text_delta', text: 'hello' } }),
    sse('content_block_stop', { index: 0 }),
    sse('message_delta', { delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 1 } }),
    sse('message_stop', {}),
  ] }])
  const ctx = new Context()
  onTestFinished(() => ctx.fiber.dispose())
  let providers: Record<string, PiAiProviderProfile> = {}
  let selection: unknown
  ctx.provide('credentials', { describe: async () => ({ configured: true }) } as never)
  ctx.provide('agentDefaultModel', { saveSelection: async (value: unknown) => { selection = value } } as never)
  ctx.provide('configEditor', {
    entries: () => [{ options: { id: 'llm-pi-ai' } }],
    edit: async (
      _entry: object,
      update: (current: { providers: Record<string, PiAiProviderProfile> }) => { providers: Record<string, PiAiProviderProfile> },
    ) => {
      providers = update({ providers }).providers
    },
  } as never)
  const setup: ModelSetup = { provider: 'claude-relay', model: 'claude-opus-5-5', baseURL: server.url, api: 'anthropic-messages',
    local: false, contextWindow: 200000, maxTokens: 8000, thinking: 'off' }
  await configureModel(ctx, setup)
  expect(providers['claude-relay']).toMatchObject({ reasoning: 'low', cacheRetention: 'long' })
  expect(providers['claude-relay']?.models?.[0]).not.toHaveProperty('reasoningEfforts')
  expect(selection).toMatchObject({ reasoningEffort: 'low' })
  const adapter = new PiAiAdapter({ profiles: () => resolveProfiles(providers), resolveApiKey: () => Promise.resolve('test-key'), auth: memoryAuth() })
  const chunks = []
  for await (const chunk of adapter.stream({ provider: setup.provider, model: setup.model, maxTokens: 8000,
    messages: [createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Return a concise checkpoint.' }] })] })) chunks.push(chunk)
  expect(chunks.at(-1)).toMatchObject({ type: 'finish', reason: { kind: 'stop' } })
  expect(server.requests[0]).toMatchObject({ thinking: { type: 'adaptive' } })
  expect(JSON.stringify(server.requests[0])).toContain('"effort":"low"')
  expect(JSON.stringify(server.requests[0])).toContain('"ttl":"1h"')
})

it('saves both Claude presets under one Anthropic key with the catalog efforts', async () => {
  const ctx = new Context()
  onTestFinished(() => ctx.fiber.dispose())
  let providers: Record<string, PiAiProviderProfile> = {}
  ctx.provide('credentials', { describe: async () => ({ configured: true }) } as never)
  ctx.provide('agentDefaultModel', { saveSelection: async () => undefined } as never)
  ctx.provide('configEditor', {
    entries: () => [{ options: { id: 'llm-pi-ai' } }],
    edit: async (
      _entry: object,
      update: (current: { providers: Record<string, PiAiProviderProfile> }) => { providers: Record<string, PiAiProviderProfile> },
    ) => {
      providers = update({ providers }).providers
    },
  } as never)
  await configureModel(ctx, CLAUDE_OPUS)
  await configureModel(ctx, CLAUDE_HAIKU)
  for (const preset of [CLAUDE_OPUS, CLAUDE_HAIKU]) {
    expect(providers[preset.provider]).toMatchObject({ apiKeyEnv: 'ANTHROPIC_API_KEY', api: 'anthropic-messages', reasoning: 'high', cacheRetention: 'long' })
    expect(providers[preset.provider]?.models?.[0]).not.toHaveProperty('reasoningEfforts')
  }
  expect(providers[CLAUDE_HAIKU.provider]).toMatchObject({ displayName: 'Claude Haiku', defaultContextWindow: 100000 })
  const adapter = new PiAiAdapter({ profiles: () => resolveProfiles(providers), resolveApiKey: () => Promise.resolve('test-key'), auth: memoryAuth() })
  expect((await adapter.resolveModel(CLAUDE_HAIKU.provider, CLAUDE_HAIKU.model)).reasoning?.efforts.map(effort => String(effort.id)))
    .toEqual(['low', 'medium', 'high', 'xhigh', 'max'])
})
