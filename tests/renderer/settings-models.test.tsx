// @vitest-environment happy-dom
/** Models settings keep the default model, the model form, the global prompt and the project budget attached to their owners. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { BudgetPreview, ModelSetup, ModelsStatus } from '../../src/shared/rpc.ts'
import { ModelsSection } from '../../src/renderer/settings/ModelsSection.tsx'
import { settingsMessages } from '../../src/renderer/settings/messages.ts'
import {
  all, button, choose, cleanup, click, control, deferred, emit, fakeHost, findButton, handle, hasText, render, setBridge, strataBridge,
  strataStatus, toast, type, waitFor,
} from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const zh = settingsMessages.zh
const en = settingsMessages.en
const workspace = { workspaceId: 'a' as WorkspaceId, path: '/project-a', title: 'A' }
const official: ModelSetup = { provider: 'official', model: 'large', baseURL: 'https://example.test', contextWindow: 1048576, maxTokens: 393216, local: false }

function modelsStatus(change: Partial<ModelsStatus> = {}): ModelsStatus {
  return { models: [official], credentials: [], selected: { provider: 'official', model: 'large' }, thinkingLevels: { official: ['off', 'low', 'high', 'max'] }, presets: [],
    globalPrompt: { text: '', maxChars: 40 }, ...change }
}
const preview: BudgetPreview = { model: 'large', tokens: 1700, contextWindow: 1048576, inputLimit: 600000, outputTokens: 393216,
  marginTokens: 16384, kind: 'estimated', breakdown: { system: 900, tools: 500, extensions: 0, instructions: 100, memory: 0, history: 200, framing: 0 } }

let status: ModelsStatus
const calls = (method: string) => fakeHost.call.mock.calls.filter(([name]) => name === method).map(([, params]) => params)

beforeEach(async () => {
  status = modelsStatus()
  handle('models.status', () => status)
  handle('budget.preview', () => preview)
  handle('models.configure', setup => ({ provider: setup.provider, model: setup.model }))
  handle('models.discover', () => [])
  handle('models.probe', () => ({ stream: true, toolCall: true }))
  await emit('prefs.changed', { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true })
})
afterEach(async () => { await cleanup(); setBridge('__RAINY_STRATA_NATIVE__', undefined); delete window.__RAINY_WORKBENCH_CONFIG__ })

const value = (label: string): string => control<HTMLInputElement>(label).value
async function addModel(): Promise<void> {
  await click(button(zh.settingsAddModel, document.querySelector('[data-default-model]')!))
}

it('loads the default model and starts a new local model at 100k without inheriting endpoint fields or output reservation', async () => {
  await render(<ModelsSection workspace={workspace} />)
  await waitFor(() => { expect(value(zh.settingsContext)).toBe('1048576') })
  expect(hasText('编辑 official/large')).toBe(true)
  await addModel()
  for (const label of [zh.settingsProvider, zh.settingsBaseUrl, zh.settingsModelId, zh.settingsOutput, zh.settingsApiKey]) expect(value(label)).toBe('')
  expect(value(zh.settingsContext)).toBe('100000')
  await type(control(zh.settingsProvider), 'local-test')
  await type(control(zh.settingsBaseUrl), 'http://127.0.0.1:1234/v1')
  await type(control(zh.settingsModelId), 'small-model')
  await type(control(zh.settingsContext), '32768')
  await click(button(zh.settingsSaveModel))
  await waitFor(() => { expect(calls('models.configure')).toHaveLength(1) })
  expect(calls('models.configure')[0]).toEqual({ provider: 'local-test', baseURL: 'http://127.0.0.1:1234/v1', model: 'small-model',
    contextWindow: 32768, local: true, api: 'openai-completions', thinking: 'off', thinkingFormat: 'openai', maxTokensField: 'max_tokens' })
  expect(toast).toHaveBeenCalledWith(zh.settingsSaved, { tone: 'success' })
  expect(hasText('编辑 local-test/small-model')).toBe(true)
})

it('switches untouched new-model defaults between the configured local and API windows, preserving explicit values', async () => {
  window.__RAINY_WORKBENCH_CONFIG__ = { readyTimeoutMs: 15000, flushTimeoutMs: 15000, editorPollMs: 60000, editorStateDebounceMs: 500,
    executionPollMs: 200, editorMaxOutputCharacters: 1024, editorMaxRetainedWorkspaces: 8, editorTerminalCols: 80, editorTerminalRows: 24,
    localModelContextWindow: 120000, apiModelContextWindow: 900000 }
  await render(<ModelsSection workspace={workspace} />)
  await addModel()
  expect(value(zh.settingsContext)).toBe('120000')
  await click(button(zh.settingsApiModel))
  expect(value(zh.settingsContext)).toBe('900000')
  await click(button(zh.settingsLocalModel))
  expect(value(zh.settingsContext)).toBe('120000')
  await type(control(zh.settingsContext), '65536')
  await click(button(zh.settingsApiModel))
  expect(value(zh.settingsContext)).toBe('65536')
})

it.each([{ locale: 'zh' as const, copy: zh }, { locale: 'en' as const, copy: en }])('discovers models before a model or context is entered in $locale', async ({ locale, copy }) => {
  await emit('prefs.changed', { locale, theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true })
  handle('models.discover', () => [{ id: 'available-small' }, { id: 'available-large', contextWindow: 131072 }])
  await render(<ModelsSection workspace={workspace} />)
  await click(button(copy.settingsAddModel, document.querySelector('[data-default-model]')!))
  await type(control(copy.settingsProvider), 'local-test')
  await type(control(copy.settingsBaseUrl), 'http://127.0.0.1:1234/v1')
  await type(control(copy.settingsContext), '')
  await click(button(copy.settingsDiscoverModels))
  await waitFor(() => { expect(calls('models.discover')).toEqual([{ provider: 'local-test', baseURL: 'http://127.0.0.1:1234/v1', api: 'openai-completions' }]) })
  expect(calls('models.configure')).toEqual([])
  expect(calls('models.probe')).toEqual([])
  const result = document.querySelector('[data-model-form] [role="status"]')!
  expect([...result.querySelectorAll('button')].map(pill => pill.textContent)).toEqual(['available-small', 'available-large'])
  expect(result.textContent).toContain(copy.settingsDiscovered.replace('{count}', '2'))
  await click(button(copy.settingsSaveModel))
  await waitFor(() => { expect(toast).toHaveBeenCalledWith(copy.settingsRequiredModel) })
  expect(calls('models.configure')).toEqual([])
  await click(button('available-large'))
  expect(value(copy.settingsModelId)).toBe('available-large')
  expect(value(copy.settingsContext)).toBe('')
})

it('fills an untouched context window from a discovered model and reports probe results', async () => {
  handle('models.discover', () => [{ id: 'served', contextWindow: 131072 }])
  handle('models.probe', () => ({ stream: true, toolCall: false, text: 'pong' }))
  await render(<ModelsSection workspace={workspace} />)
  await addModel()
  await type(control(zh.settingsProvider), 'local-test')
  await type(control(zh.settingsBaseUrl), 'http://127.0.0.1:1234/v1')
  await click(button(zh.settingsDiscoverModels))
  await click(button('served'))
  expect(value(zh.settingsModelId)).toBe('served')
  expect(value(zh.settingsContext)).toBe('131072')
  await click(button(zh.settingsProbeModel))
  await waitFor(() => { expect(document.querySelector('[data-model-form] [role="status"]')?.textContent).toBe('流式输出：通过\n工具调用：未验证\n模型回复：pong') })
})

it('requires connection details before model discovery', async () => {
  await render(<ModelsSection workspace={workspace} />)
  await addModel()
  await click(button(zh.settingsDiscoverModels))
  await waitFor(() => { expect(toast).toHaveBeenCalledWith(zh.settingsRequiredDiscovery) })
  expect(calls('models.discover')).toEqual([])
})

it('rejects a context window or output limit that is not a positive integer', async () => {
  await render(<ModelsSection workspace={workspace} />)
  await type(control(zh.settingsOutput), '12.5')
  await click(button(zh.settingsSaveModel))
  await waitFor(() => { expect(toast).toHaveBeenCalledWith(zh.settingsInvalidNumber) })
  expect(calls('models.configure')).toEqual([])
})

it('selects a saved model and the default reasoning effort, shows stored keys and removes after confirmation', async () => {
  const other: ModelSetup = { provider: 'local', model: 'small', baseURL: 'http://127.0.0.1:8081/v1', contextWindow: 32768, local: true, thinking: 'high' }
  status = modelsStatus({ models: [official, other], credentials: ['official'] })
  handle('models.select', (selection) => { status = { ...status, selected: selection }; return status })
  handle('models.remove', ({ provider }) => { status = { ...status, models: status.models.filter(model => model.provider !== provider) }; return status })
  await render(<ModelsSection workspace={workspace} />)
  const row = (key: string): HTMLElement => document.querySelector<HTMLElement>(`[data-model="${key}"]`)!
  await waitFor(() => { expect(row('local/small')).not.toBeNull() })
  expect(row('official/large').textContent).toContain(zh.modelDefault)
  expect(row('official/large').textContent).toContain(zh.modelKeyStored)
  expect(row('local/small').textContent).not.toContain(zh.modelKeyStored)
  expect(control(zh.settingsApiKeyStored).getAttribute('type')).toBe('password')
  await click(button(zh.modelUseDefault, row('local/small')))
  expect(calls('models.select')).toEqual([{ provider: 'local', model: 'small' }])
  await waitFor(() => { expect(row('local/small').textContent).toContain(zh.modelDefault) })
  await choose('local/small 的推理档位', 'max')
  expect(calls('models.select')[1]).toEqual({ provider: 'local', model: 'small', thinking: 'max' })
  await click(button(zh.modelEdit, row('local/small')))
  expect(value(zh.settingsModelId)).toBe('small')
  expect(value(zh.settingsApiKey)).toBe('')
  await click(button(zh.modelRemove, row('local/small')))
  expect(calls('models.remove')).toEqual([])
  await click(button(zh.modelRemoveConfirm, row('local/small')))
  await waitFor(() => { expect(document.querySelector('[data-model="local/small"]')).toBeNull() })
  expect(calls('models.remove')).toEqual([{ provider: 'local' }])
  expect(toast).toHaveBeenCalledWith('已删除 local/small', { tone: 'success' })
  expect(value(zh.settingsModelId)).toBe('')
})

it('loads a preset into a new model form', async () => {
  status = modelsStatus({ presets: [{ name: 'DeepSeek', model: { provider: 'rainy-deepseek', baseURL: 'https://api.deepseek.com', model: 'deepseek-flash',
    contextWindow: 1000000, maxTokens: 393216, local: false, api: 'openai-responses', thinking: 'max' } }] })
  await render(<ModelsSection workspace={workspace} />)
  await click(button('载入 DeepSeek 预设'))
  expect(value(zh.settingsProvider)).toBe('rainy-deepseek')
  expect(value(zh.settingsContext)).toBe('1000000')
  expect(hasText(zh.settingsAddModel, document.querySelector('[data-model-form]')!)).toBe(true)
  expect(button(zh.settingsApiModel).getAttribute('aria-selected')).toBe('true')
})

it('does not let a status read issued before a save replace the saved model', async () => {
  const earlier = deferred<ModelsStatus>()
  let reads = 0
  handle('models.status', () => { reads++; return reads === 1 ? earlier.promise : status })
  handle('models.configure', ({ apiKey: _key, ...setup }) => {
    status = modelsStatus({ models: [official, setup], selected: { provider: setup.provider, model: setup.model } })
    return { provider: setup.provider, model: setup.model }
  })
  await render(<ModelsSection workspace={workspace} />)
  await addModel()
  await type(control(zh.settingsProvider), 'local')
  await type(control(zh.settingsBaseUrl), 'http://127.0.0.1:8081/v1')
  await type(control(zh.settingsModelId), 'new-model')
  await click(button(zh.settingsSaveModel))
  await waitFor(() => { expect(document.querySelector('[data-model="local/new-model"]')?.textContent).toContain(zh.modelDefault) })
  earlier.resolve(modelsStatus())
  await waitFor(() => { expect(reads).toBe(2) })
  expect(document.querySelector('[data-model="local/new-model"]')?.textContent).toContain(zh.modelDefault)
  await emit('models.changed', modelsStatus({ models: [] }))
  expect(hasText(zh.modelsEmpty)).toBe(true)
})

it('selects the actual Strata configuration after the desktop connection without saving it a second time', async () => {
  const running = { ...strataStatus(), phase: 'running' as const,
    server: { baseURL: 'http://127.0.0.1:8081/v1', model: 'served-model', contextWindow: 65536, loaded: true, owned: true, authenticationRequired: false } }
  const bridge = strataBridge(running)
  setBridge('__RAINY_STRATA_NATIVE__', bridge)
  const served: ModelSetup = { provider: 'rainy-strata', model: 'served-model', baseURL: 'http://127.0.0.1:8081/v1', contextWindow: 65536,
    maxTokens: 8192, api: 'openai-completions', local: true, thinking: 'high', thinkingFormat: 'openai', maxTokensField: 'max_tokens' }
  bridge.connect.mockImplementation(async () => {
    status = modelsStatus({ models: [served], selected: { provider: 'rainy-strata', model: 'served-model' } })
    return { provider: 'rainy-strata', model: 'served-model' }
  })
  await render(<ModelsSection workspace={workspace} />)
  await waitFor(() => { expect(button(zh.strataConnect).disabled).toBe(false) })
  await click(button(zh.strataConnect))
  await waitFor(() => { expect(value(zh.settingsModelId)).toBe('served-model') })
  expect(value(zh.settingsContext)).toBe('65536')
  expect(value(zh.settingsOutput)).toBe('8192')
  expect(hasText('编辑 rainy-strata/served-model')).toBe(true)
  expect(toast).toHaveBeenCalledWith(zh.strataConnected, { tone: 'success' })
  expect(calls('models.configure')).toEqual([])
})

it('waits for a selected model before requesting a project budget preview', async () => {
  status = modelsStatus({ selected: null })
  await render(<ModelsSection workspace={workspace} />)
  expect(hasText(zh.settingsEmptyBudget)).toBe(true)
  expect(calls('budget.preview')).toEqual([])
})

it('asks for a project folder before previewing a budget', async () => {
  await render(<ModelsSection workspace={null} />)
  expect(document.querySelector('[data-budget]')?.textContent).toContain(zh.settingsOpenProject)
  expect(calls('budget.preview')).toEqual([])
})

it('previews the current project before its first message and states the estimate limits', async () => {
  await render(<ModelsSection workspace={workspace} />)
  await waitFor(() => { expect(hasText(zh.settingsPreviewBudget)).toBe(true) })
  expect(calls('budget.preview')).toEqual([{ workspaceId: 'a' }])
  const card = document.querySelector('[data-budget]')!
  expect(card.textContent).toContain('1,700 / 1,048,576 tokens（估计）')
  expect(card.textContent).toContain('估算分项：系统 900 · 工具 500')
  expect(hasText(zh.settingsPreviewDispatch)).toBe(true)
  expect(hasText(zh.settingsPreviewAttachments)).toBe(true)
  await click(button(zh.settingsRefresh, card))
  expect(calls('budget.preview')).toHaveLength(2)
})

it.each([{ locale: 'zh' as const, copy: zh }, { locale: 'en' as const, copy: en }])('puts the default model first and keeps the local engine collapsed in $locale', async ({ locale, copy }) => {
  await emit('prefs.changed', { locale, theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true })
  setBridge('__RAINY_STRATA_NATIVE__', strataBridge())
  const { container } = await render(<ModelsSection workspace={workspace} />)
  const page = container.querySelector('[data-settings-section="models"]')!
  expect(page.querySelector('h2')?.textContent).toBe(copy.settingsModels)
  const blocks = [...page.children].slice(1)
  expect(blocks.map(block => block.querySelector('h3, summary > span')?.textContent))
    .toEqual([copy.settingsDefaultModel, copy.modelEditTitle.replace('{name}', 'official/large'), copy.settingsGlobalPrompt, copy.settingsContextBudget, copy.strataTitle])
  expect(page.querySelector<HTMLDetailsElement>('details[data-strata]')?.open).toBe(false)
})

it('saves the global prompt within the configured limit and refreshes the budget estimate', async () => {
  handle('prompt.global', ({ text }) => { status = { ...status, globalPrompt: { text: text.trim(), maxChars: 40 } }; return status })
  await render(<ModelsSection workspace={workspace} />)
  await waitFor(() => { expect(calls('budget.preview')).toHaveLength(1) })
  const prompt = control<HTMLTextAreaElement>(zh.settingsGlobalPrompt)
  const save = (): HTMLButtonElement => button(zh.settingsSave, document.querySelector('[data-global-prompt]')!)
  expect(prompt.maxLength).toBe(40)
  expect(save().disabled).toBe(true)
  await type(prompt, '  Answer in Chinese.  ')
  expect(hasText('22 / 40 字')).toBe(true)
  await click(save())
  await waitFor(() => { expect(calls('prompt.global')).toEqual([{ text: '  Answer in Chinese.  ' }]) })
  await waitFor(() => { expect(prompt.value).toBe('Answer in Chinese.') })
  expect(save().disabled).toBe(true)
  expect(toast).toHaveBeenCalledWith(zh.settingsSaved, { tone: 'success' })
  await waitFor(() => { expect(calls('budget.preview')).toHaveLength(2) })
})

it('keeps an unsaved global prompt draft when the Host rejects it', async () => {
  handle('prompt.global', () => { throw new Error('全局提示词超过 40 字符上限') })
  await render(<ModelsSection workspace={workspace} />)
  await waitFor(() => { expect(control<HTMLTextAreaElement>(zh.settingsGlobalPrompt).disabled).toBe(false) })
  const prompt = control<HTMLTextAreaElement>(zh.settingsGlobalPrompt)
  await type(prompt, 'Draft instructions')
  await click(button(zh.settingsSave, document.querySelector('[data-global-prompt]')!))
  await waitFor(() => { expect(toast).toHaveBeenCalledWith('全局提示词超过 40 字符上限') })
  expect(prompt.value).toBe('Draft instructions')
  expect(button(zh.settingsSave, document.querySelector('[data-global-prompt]')!).disabled).toBe(false)
})

it('shows a status read failure and stops listening after unmount', async () => {
  handle('models.status', () => { throw new Error('Host unavailable') })
  const view = await render(<ModelsSection workspace={workspace} />)
  await waitFor(() => { expect(document.querySelector('[role="alert"]')?.textContent).toBe('Host unavailable') })
  expect(findButton(zh.modelUseDefault)).toBeUndefined()
  await view.unmount()
  expect(all('[data-settings-section]')).toEqual([])
})
