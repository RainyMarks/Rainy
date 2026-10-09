/** The selected execution Host verifies Strata before changing the active local model. */
import { createServer } from 'node:http'
import type { AddressInfo } from 'node:net'
import { Context } from '@deepseek-ai/cordis'
import { afterEach, expect, it, vi } from 'vitest'
import { connectStrataModel } from '../src/models.ts'

const cleanup: (() => Promise<void>)[] = []
afterEach(async () => { for (const close of cleanup.splice(0).reverse()) await close() })

async function fixture(previous = false, savedOutput = 4096) {
  const ctx = new Context()
  cleanup.push(async () => { await ctx.fiber.dispose() })
  const health = { status: 'ok', service: 'strata', loaded: true, api_key: false,
    model: 'qwen3.8-flash-next', max_context: 32768 }
  const server = createServer((_request, response) => {
    response.setHeader('Content-Type', 'application/json')
    response.end(JSON.stringify(health))
  })
  await new Promise<void>((resolve, reject) => {
    server.once('error', reject)
    server.listen(0, '127.0.0.1', () => { server.off('error', reject); resolve() })
  })
  cleanup.push(() => new Promise<void>((resolve, reject) => { server.close((error) => { if (error) reject(error); else resolve() }) }))
  const port = (server.address() as AddressInfo).port
  const baseURL = `http://127.0.0.1:${port}/v1`
  let config: object = { providers: previous ? { 'rainy-strata': { displayName: '本地模型', baseURL,
    api: 'openai-completions', reasoning: 'low', models: [{ id: health.model, contextWindow: 32768, maxTokens: savedOutput }] } } : {} }
  const entry = { options: { id: 'llm-pi-ai', get config() { return config } } }
  const edit = vi.fn(async (_entry: object, update: (current: object) => object) => { config = update(config) })
  const set = vi.fn(async () => {})
  const saveSelection = vi.fn(async () => {})
  ctx.provide('configEditor', { entries: () => [entry], edit } as never)
  ctx.provide('credentials', { describe: async () => ({ configured: false }), set } as never)
  ctx.provide('agentDefaultModel', { saveSelection } as never)
  return { ctx, health, baseURL, edit, set, saveSelection, config: () => config }
}

it('uses the loaded local server model and context instead of caller-supplied metadata', async () => {
  const f = await fixture()
  await expect(connectStrataModel(f.ctx, { baseURL: f.baseURL, model: 'untrusted', contextWindow: 1000000 }))
    .resolves.toEqual({ provider: 'rainy-strata', model: 'qwen3.8-flash-next' })
  expect(f.config()).toMatchObject({ providers: { 'rainy-strata': {
    baseURL: f.baseURL, api: 'openai-completions', defaultContextWindow: 32768,
    compat: { thinkingFormat: 'openai', maxTokensField: 'max_tokens' },
  } } })
  expect(f.saveSelection).toHaveBeenCalledExactlyOnceWith({ provider: 'rainy-strata', model: 'qwen3.8-flash-next' })
  expect(f.set).toHaveBeenCalledOnce()
})

it('preserves the saved Strata output and reasoning preferences', async () => {
  const f = await fixture(true)
  await connectStrataModel(f.ctx, { baseURL: f.baseURL })
  expect(f.config()).toMatchObject({ providers: { 'rainy-strata': { defaultMaxTokens: 4096, reasoning: 'low' } } })
})

it('requires an explicit context-bound output adjustment when the model context shrinks', async () => {
  const f = await fixture(true, 16000)
  f.health.max_context = 8192
  await expect(connectStrataModel(f.ctx, { baseURL: f.baseURL })).rejects.toThrow('输出预留过大')
  expect(f.edit).not.toHaveBeenCalled()
  await connectStrataModel(f.ctx, { baseURL: f.baseURL, maxTokens: 1310, expectedContextWindow: 8192 })
  expect(f.config()).toMatchObject({ providers: { 'rainy-strata': {
    defaultMaxTokens: 1310, defaultContextWindow: 8192, reasoning: 'low',
  } } })
})

it('rejects a confirmed budget after the server context changes', async () => {
  const f = await fixture(true, 16000)
  f.health.max_context = 8192
  await expect(connectStrataModel(f.ctx, { baseURL: f.baseURL, maxTokens: 1310, expectedContextWindow: 16384 }))
    .rejects.toThrow('实际上下文已改变')
  expect(f.edit).not.toHaveBeenCalled()
  expect(f.set).not.toHaveBeenCalled()
})

it.each(['unloaded', 'other-service', 'authenticated', 'remote'] as const)('leaves model and credentials unchanged for %s endpoints', async (kind) => {
  const f = await fixture()
  if (kind === 'unloaded') f.health.loaded = false
  if (kind === 'other-service') f.health.service = 'another-service'
  if (kind === 'authenticated') f.health.api_key = true
  await expect(connectStrataModel(f.ctx, { baseURL: kind === 'remote' ? 'https://example.test/v1' : f.baseURL }))
    .rejects.toThrow('不会切换到云端')
  expect(f.edit).not.toHaveBeenCalled()
  expect(f.set).not.toHaveBeenCalled()
  expect(f.saveSelection).not.toHaveBeenCalled()
})
