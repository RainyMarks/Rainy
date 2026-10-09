/** Discovery validates endpoint data without requiring or changing a model configuration. */
import { Context } from '@deepseek-ai/cordis'
import { afterEach, expect, it, vi } from 'vitest'
import { configureModel, discoverModels, parseModelDiscovery, parseModelSetup, probeModel, DEEPSEEK_FLASH } from '../src/models.ts'

const contexts: Context[] = []
afterEach(async () => { for (const ctx of contexts.splice(0)) await ctx.fiber.dispose() })

const connection = { provider: 'local-test', baseURL: 'http://127.0.0.1:1234/v1', api: 'openai-completions' } as const

function fixture() {
  const ctx = new Context()
  contexts.push(ctx)
  const discover = vi.fn<Context['llm']['discoverModels']>(async () => [{ id: 'available-model' }])
  ctx.provide('llm', { discoverModels: discover } as never)
  return { ctx, discover }
}

it('lists models using only validated connection fields without saving credentials or selecting a model', async () => {
  const { ctx, discover } = fixture()
  const raw = { ...connection, baseURL: connection.baseURL + '/', apiKey: 'test-request-key' }
  expect(parseModelDiscovery(raw)).toEqual({ ...connection, apiKey: 'test-request-key' })
  await expect(discoverModels(ctx, raw)).resolves.toEqual({ data: [{ id: 'available-model' }] })
  expect(discover).toHaveBeenCalledExactlyOnceWith('llm-pi-ai', { ...connection, apiKey: 'test-request-key' }, expect.any(AbortSignal))
})

it('does not require a key for a local discovery request', async () => {
  const { ctx, discover } = fixture()
  await discoverModels(ctx, connection)
  expect(discover).toHaveBeenCalledExactlyOnceWith('llm-pi-ai', connection, expect.any(AbortSignal))
})

it.each([
  { field: 'provider', value: 'invalid provider' },
  { field: 'baseURL', value: 'not-a-url' },
  { field: 'baseURL', value: 'file:///local/models' },
  { field: 'baseURL', value: 'https://user:secret@example.test' },
  { field: 'baseURL', value: 'https://example.test?key=secret' },
  { field: 'baseURL', value: 'https://example.test#secret' },
  { field: 'api', value: 'unsupported' },
  { field: 'apiKey', value: 123 },
])('rejects invalid discovery $field before contacting the adapter', async ({ field, value }) => {
  const { ctx, discover } = fixture()
  const result = discoverModels(ctx, { ...connection, [field]: value })
  await expect(result).rejects.toThrow()
  await expect(result).rejects.not.toThrow('secret')
  expect(discover).not.toHaveBeenCalled()
})

it('retains full model validation for saving and probing', async () => {
  const { ctx, discover } = fixture()
  expect(parseModelSetup(DEEPSEEK_FLASH)).toEqual(DEEPSEEK_FLASH)
  await expect(configureModel(ctx, connection)).rejects.toThrow('模型 ID')
  await expect(probeModel(ctx, connection)).rejects.toThrow('模型 ID')
  expect(() => parseModelSetup({ ...DEEPSEEK_FLASH, contextWindow: undefined })).toThrow('实际上下文长度')
  expect(() => parseModelDiscovery({ ...connection, api: undefined })).toThrow('API')
  expect(discover).not.toHaveBeenCalled()
})
