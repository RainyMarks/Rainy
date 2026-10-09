/** Model settings validate connections, list models, share one Anthropic key across the Claude presets, and send the chosen reasoning level. */
import { afterEach, expect, it } from 'vitest'
import { Settings } from '../../src/host/settings.ts'
import { effectiveThinking, thinkingLevels } from '../../src/host/agent/llm.ts'
import { CLAUDE_HAIKU, CLAUDE_OPUS, Models, parseDiscovery, parseSetup } from '../../src/host/agent/models.ts'
import { FakeOpenAI, tempHost } from './agent-fixtures.ts'

const cleanups: (() => Promise<void>)[] = []
afterEach(async () => { for (const cleanup of cleanups.splice(0).reverse()) await cleanup() })

async function models(env: NodeJS.ProcessEnv = {}): Promise<{ models: Models; settings: Settings }> {
  const host = await tempHost()
  cleanups.push(host.cleanup)
  const settings = new Settings(host.env.home, env)
  await settings.load()
  return { models: new Models(settings), settings }
}

it('validates connection fields without echoing credentials', () => {
  expect(parseDiscovery({ provider: 'local', baseURL: 'http://127.0.0.1:8080/v1/', api: 'openai-completions' }))
    .toEqual({ provider: 'local', baseURL: 'http://127.0.0.1:8080/v1', api: 'openai-completions' })
  expect(() => parseDiscovery({ provider: 'x', baseURL: 'https://user:secret@example.test', api: 'openai-completions' })).toThrow('不能包含密钥')
  expect(() => parseDiscovery({ provider: 'x', baseURL: 'https://example.test?key=secret', api: 'openai-completions' })).toThrow('不能包含密钥')
  expect(() => parseDiscovery({ provider: 'Bad Name', baseURL: 'https://example.test', api: 'openai-completions' })).toThrow('供应商 ID')
  expect(() => parseSetup({ provider: 'x', baseURL: 'https://example.test', model: 'm', contextWindow: '1M', local: false })).toThrow('上下文长度')
  expect(() => parseSetup({ provider: 'x', baseURL: 'https://example.test', model: 'm', contextWindow: 32768, local: false, thinking: 'medium' })).toThrow('推理档位')
  expect(parseSetup({ provider: 'x', baseURL: 'https://example.test', model: ' m ', contextWindow: 32768, local: true }))
    .toMatchObject({ model: 'm', api: 'openai-completions', local: true })
})

it('offers only the reasoning levels a model accepts', () => {
  expect(thinkingLevels(CLAUDE_OPUS)).not.toContain('off')
  expect(effectiveThinking(CLAUDE_OPUS, 'off')).toBe('low')
  expect(thinkingLevels({ api: 'openai-completions', model: 'qwen', thinking: undefined })).toEqual(['off'])
  expect(thinkingLevels({ api: 'openai-completions', model: 'qwen', thinking: 'high' })).toEqual(['off', 'low', 'high', 'max'])
})

it('lists a local server’s models without a key and saves nothing', async () => {
  const server = new FakeOpenAI([])
  await server.start()
  cleanups.push(() => server.stop())
  const { models: target, settings } = await models()
  expect(await target.discover({ provider: 'local', baseURL: server.baseURL, api: 'openai-completions' })).toEqual([{ id: 'fake-model', contextWindow: 32768 }])
  expect(settings.get().models).toEqual([])
  expect(target.status().selected).toBeNull()
})

it('stores one Anthropic key for both Claude presets and reports their reasoning levels', async () => {
  const { models: target, settings } = await models()
  await target.configure({ ...CLAUDE_OPUS, apiKey: 'sk-ant-test' })
  await target.configure(CLAUDE_HAIKU)
  expect(settings.apiKey('rainy-claude-haiku')).toBe('sk-ant-test')
  const status = target.status()
  expect(status.credentials).toEqual(expect.arrayContaining(['rainy-claude', 'rainy-claude-haiku']))
  expect(status.thinkingLevels['rainy-claude']).not.toContain('off')
  expect(status.selected).toMatchObject({ provider: 'rainy-claude-haiku' })
  expect(JSON.stringify(settings.get())).not.toContain('sk-ant-test')
})

it('sends the reasoning level a chat selects in the provider’s wire format', async () => {
  const server = new FakeOpenAI([{ text: 'ok' }])
  await server.start()
  cleanups.push(() => server.stop())
  const { models: target } = await models()
  await target.configure({ provider: 'qwen-local', baseURL: server.baseURL, model: 'qwen3', contextWindow: 32768, local: true, api: 'openai-completions', thinking: 'high', thinkingFormat: 'qwen' })
  const resolved = target.resolve({ provider: 'qwen-local', model: 'qwen3', thinking: 'high' })
  expect(resolved?.thinking).toBe('high')
  const reply = await target.complete(resolved!, 'hello', [], 10_000)
  expect(reply.stopReason).toBe('stop')
  expect(server.requests[0]).toMatchObject({ model: 'qwen3', enable_thinking: true })
})
