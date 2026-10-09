// @vitest-environment jsdom
/** Unified settings keep model limits and project memory attached to their owning selection. */
import { afterEach, expect, it, vi } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { bindSnapshotSelector } from '@deepseek-ai/dsh-client-test-runtime'
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import { ModelsSection, type SettingsSectionProps } from '../src/client/SettingsSections.tsx'
import { MemorySection, RuntimeSection } from '../src/client/ProjectSettingsSections.tsx'
import type { ProjectMemoryStatus } from '../src/client/settings-protocol.ts'
import type { SettingsSnapshot } from '../src/client/settings-controller.ts'
import type { StrataSnapshot } from '../src/client/strata-controller.ts'
import { IdeModel } from '../src/client/ide-model.ts'
import type { WorkspaceId } from '../src/ide-files-protocol.ts'
import type { RuntimeSnapshot } from '../src/runtime-protocol.ts'
import { en, zh } from '../src/client/locales.ts'
import { globalProps } from './global-props.client.ts'
import { strataStatus } from './strata-fixture.client.ts'

const owners: IdeModel[] = []
afterEach(() => { cleanup(); for (const owner of owners.splice(0)) owner.dispose(); vi.restoreAllMocks() })
const t = ((key: keyof typeof zh, values?: Record<string, string | number>) =>
  zh[key].replace(/\{(\w+)\}/g, (match, name: string) => values?.[name] === undefined ? match : String(values[name]))) as TranslateNS<'rainy'>
const emptyMemory = (): ProjectMemoryStatus => ({ enabled: true, generationEnabled: true, revision: 4, items: [] })

function fixture() {
  const ide = new IdeModel({ request: vi.fn() }, { debounceMs: 60_000, pollMs: 60_000, restoreSession: vi.fn(async () => {}) })
  owners.push(ide)
  ide.state.set({ ...ide.state.getSnapshot(), phase: 'ready', workspace: { workspaceId: 'a' as WorkspaceId, path: '/project-a', title: 'A' } })
  const state = createSnapshotStore<SettingsSnapshot>({ loading: false, error: '', status: {
    models: [{ provider: 'official', model: 'large', baseURL: 'https://example.test', contextWindow: 1048576, maxTokens: 393216, local: false }],
    selected: { provider: 'official', model: 'large' }, budgets: [], sessions: [], tools: [],
    globalPrompt: { text: '', maxChars: 40 },
  } })
  const strata = createSnapshotStore<StrataSnapshot>({ available: false, loading: false, error: '' })
  const props: SettingsSectionProps = { ...globalProps, t, close: vi.fn(), useSettings: bindSnapshotSelector(state),
    useStrata: bindSnapshotSelector(strata),
    strataRefresh: vi.fn(async () => {}), strataSave: vi.fn(async () => undefined), strataStart: vi.fn(async () => {}),
    strataStop: vi.fn(async () => {}), strataChoose: vi.fn(async () => null), strataConnect: vi.fn(async () => undefined),
    useIde: bindSnapshotSelector(ide.state), pollMs: 60_000, localModelContextWindow: 100000, apiModelContextWindow: 1000000,
    notify: vi.fn(), refresh: vi.fn(async () => {}),
    previewBudget: vi.fn<SettingsSectionProps['previewBudget']>(async () => ({ preview: true, limitations: ['before-dispatch-estimate', 'attachments-not-included'],
      sessionId: 'preview:a', model: 'large', tokens: 1700, contextWindow: 1048576, inputLimit: 600000,
      outputTokens: 393216, marginTokens: 16384, kind: 'estimated', compacting: false })),
    configure: vi.fn<SettingsSectionProps['configure']>(async setup => ({ provider: setup.provider, model: setup.model })),
    saveGlobalPrompt: vi.fn<SettingsSectionProps['saveGlobalPrompt']>(async (text) => {
      state.set({ ...state.getSnapshot(), status: { ...state.getSnapshot().status!, globalPrompt: { text: text.trim(), maxChars: 40 } } })
    }),
    discover: vi.fn(async () => []), probe: vi.fn(async () => ({ stream: true, toolCall: true })),
    catalog: vi.fn(async () => ({ skills: [], selection: { skills: [], servers: [] }, idaAvailable: false })),
    extensions: vi.fn(async () => {}), memory: vi.fn(async () => emptyMemory()), memoryEnabled: vi.fn(async () => {}),
    memoryEdit: vi.fn(async () => {}), memoryDelete: vi.fn(async () => {}), memoryClear: vi.fn(async () => {}),
    runtime: vi.fn(),
  }
  return { props, ide, state, strata }
}

it('starts a local model at 100k without inheriting endpoint fields or output reservation', async () => {
  const { props } = fixture()
  render(<ModelsSection {...props} />)
  await screen.findByDisplayValue('1048576')
  fireEvent.click(screen.getByRole('button', { name: zh.settingsSavedModels }))
  fireEvent.click(await screen.findByRole('menuitem', { name: zh.settingsAddModel }))
  for (const label of [zh.settingsProvider, zh.settingsBaseUrl, zh.settingsModelId, zh.settingsOutput, zh.settingsApiKey])
    expect(screen.getByLabelText<HTMLInputElement>(label).value).toBe('')
  expect(screen.getByLabelText<HTMLInputElement>(zh.settingsContext).value).toBe('100000')
  fireEvent.change(screen.getByLabelText(zh.settingsProvider), { target: { value: 'local-test' } })
  fireEvent.change(screen.getByLabelText(zh.settingsBaseUrl), { target: { value: 'http://127.0.0.1:1234/v1' } })
  fireEvent.change(screen.getByLabelText(zh.settingsModelId), { target: { value: 'small-model' } })
  fireEvent.change(screen.getByLabelText(zh.settingsContext), { target: { value: '32768' } })
  fireEvent.click(screen.getByRole('button', { name: zh.settingsSaveModel }))
  await waitFor(() => { expect(props.configure).toHaveBeenCalledOnce() })
  expect(vi.mocked(props.configure).mock.calls[0]?.[0]).toEqual({ provider: 'local-test', baseURL: 'http://127.0.0.1:1234/v1', model: 'small-model',
    contextWindow: 32768, local: true, api: 'openai-completions', thinking: 'off', thinkingFormat: 'openai', maxTokensField: 'max_tokens' })
  expect(screen.queryByRole('dialog')).toBeNull()
})

it('switches untouched new-model defaults between local 100k and API one million, preserving explicit values', async () => {
  const { props } = fixture()
  render(<ModelsSection {...props} />)
  await screen.findByDisplayValue('1048576')
  fireEvent.click(screen.getByRole('button', { name: zh.settingsSavedModels }))
  fireEvent.click(await screen.findByRole('menuitem', { name: zh.settingsAddModel }))
  const choose = async (label: string) => {
    fireEvent.click(screen.getByRole('button', { name: zh.settingsRuntimeKind }))
    fireEvent.click(await screen.findByRole('menuitem', { name: label }))
  }
  await choose(zh.settingsApiModel)
  expect(screen.getByLabelText<HTMLInputElement>(zh.settingsContext).value).toBe('1000000')
  await choose(zh.settingsLocalModel)
  expect(screen.getByLabelText<HTMLInputElement>(zh.settingsContext).value).toBe('100000')
  fireEvent.change(screen.getByLabelText(zh.settingsContext), { target: { value: '65536' } })
  await choose(zh.settingsApiModel)
  expect(screen.getByLabelText<HTMLInputElement>(zh.settingsContext).value).toBe('65536')
})

it.each([{ locale: 'zh', copy: zh }, { locale: 'en', copy: en }])('discovers models before a model or context is entered in $locale', async ({ locale, copy }) => {
  const { props } = fixture()
  props.t = ((key: keyof typeof zh, values?: Record<string, string | number>) =>
    copy[key].replace(/\{(\w+)\}/g, (match, name: string) => values?.[name] === undefined ? match : String(values[name]))) as TranslateNS<'rainy'>
  vi.mocked(props.discover).mockResolvedValue([{ id: 'available-small' }, { id: 'available-large' }])
  render(<ModelsSection {...props} />)
  fireEvent.click(screen.getByRole('button', { name: copy.settingsSavedModels }))
  fireEvent.click(await screen.findByRole('menuitem', { name: copy.settingsAddModel }))
  fireEvent.change(screen.getByLabelText(copy.settingsProvider), { target: { value: 'local-test' } })
  fireEvent.change(screen.getByLabelText(copy.settingsBaseUrl), { target: { value: 'http://127.0.0.1:1234/v1' } })
  fireEvent.change(screen.getByLabelText(copy.settingsContext), { target: { value: '' } })
  fireEvent.click(screen.getByRole('button', { name: copy.settingsDiscoverModels }))
  await screen.findByRole('status')
  expect(props.discover).toHaveBeenCalledExactlyOnceWith({ provider: 'local-test', baseURL: 'http://127.0.0.1:1234/v1', api: 'openai-completions' })
  expect(props.configure).not.toHaveBeenCalled()
  expect(props.probe).not.toHaveBeenCalled()
  await expect(`${screen.getByRole('button', { name: copy.settingsDiscoverModels }).textContent}\n${screen.getByRole('status').textContent}\n`)
    .toMatchFileSnapshot(`./expected/model-discovery-${locale}.txt`)
  await waitFor(() => { expect(screen.getByRole<HTMLButtonElement>('button', { name: copy.settingsSaveModel }).disabled).toBe(false) })
  fireEvent.click(screen.getByRole('button', { name: copy.settingsSaveModel }))
  await waitFor(() => { expect(props.notify).toHaveBeenCalledWith(copy.settingsRequiredModel) })
  expect(props.configure).not.toHaveBeenCalled()
})

it('requires connection details before model discovery', async () => {
  const { props } = fixture()
  render(<ModelsSection {...props} />)
  fireEvent.click(screen.getByRole('button', { name: zh.settingsSavedModels }))
  fireEvent.click(await screen.findByRole('menuitem', { name: zh.settingsAddModel }))
  fireEvent.click(screen.getByRole('button', { name: zh.settingsDiscoverModels }))
  await waitFor(() => { expect(props.notify).toHaveBeenCalledWith(zh.settingsRequiredDiscovery) })
  expect(props.discover).not.toHaveBeenCalled()
})

it('selects the actual Strata configuration after Host connection without saving it a second time', async () => {
  const { props, state, strata } = fixture()
  const status = strataStatus()
  strata.set({ available: true, loading: false, error: '', status: { ...status, phase: 'running',
    server: { baseURL: 'http://127.0.0.1:8081/v1', model: 'served-model', contextWindow: 65536,
      loaded: true, owned: true, authenticationRequired: false } } })
  props.strataConnect = vi.fn(async () => {
    const model = { provider: 'rainy-strata', model: 'served-model', baseURL: 'http://127.0.0.1:8081/v1', contextWindow: 65536,
      maxTokens: 8192, api: 'openai-completions' as const, local: true, thinking: 'high' as const,
      thinkingFormat: 'openai' as const, maxTokensField: 'max_tokens' as const }
    state.set({ loading: false, error: '', status: { ...state.getSnapshot().status!, models: [model],
      selected: { provider: model.provider, model: model.model } } })
    return model
  })
  render(<ModelsSection {...props} />)
  fireEvent.click(screen.getByRole('button', { name: zh.strataConnect }))
  await waitFor(() => { expect(screen.getByLabelText<HTMLInputElement>(zh.settingsModelId).value).toBe('served-model') })
  expect(screen.getByLabelText<HTMLInputElement>(zh.settingsContext).value).toBe('65536')
  expect(screen.getByLabelText<HTMLInputElement>(zh.settingsOutput).value).toBe('8192')
  expect(screen.getByRole('button', { name: zh.settingsSavedModels }).textContent).toContain('rainy-strata/served-model')
  expect(props.configure).not.toHaveBeenCalled()
})

it('waits for a selected model before requesting a project budget preview', () => {
  const { props, state } = fixture()
  state.set({ ...state.getSnapshot(), status: { ...state.getSnapshot().status!, selected: { provider: '', model: '' } } })
  render(<ModelsSection {...props} />)
  expect(props.previewBudget).not.toHaveBeenCalled()
  expect(screen.getByText(zh.settingsEmptyBudget)).toBeTruthy()
})

it('previews the current project before its first message without showing another project’s latest request', async () => {
  const { props, state } = fixture()
  state.set({ ...state.getSnapshot(), status: { ...state.getSnapshot().status!, budgets: [{ sessionId: 'other-project', model: 'secret-model',
    tokens: 5000, contextWindow: 32768, inputLimit: 20000, outputTokens: 5000, marginTokens: 2000, kind: 'estimated', compacting: false }] } })
  render(<ModelsSection {...props} />)
  expect(screen.getByText(zh.settingsEmptyBudget)).toBeTruthy()
  expect(screen.queryByText(/5,000/)).toBeNull()
  await screen.findByText(zh.settingsPreviewBudget, { exact: false })
  expect(props.previewBudget).toHaveBeenCalledWith({ workspaceId: 'a', provider: 'official', model: 'large' })
  expect(screen.getByText(zh.settingsPreviewAttachments)).toBeTruthy()
})

it.each([{ locale: 'zh', copy: zh }, { locale: 'en', copy: en }])('puts the default model first and keeps the local engine collapsed in $locale', ({ copy }) => {
  const { props } = fixture()
  props.t = ((key: keyof typeof zh, values?: Record<string, string | number>) =>
    copy[key].replace(/\{(\w+)\}/g, (match, name: string) => values?.[name] === undefined ? match : String(values[name]))) as TranslateNS<'rainy'>
  const { container } = render(<ModelsSection {...props} />)
  const page = container.querySelector('[data-rainy-settings="models"]')!
  expect(page.querySelector('h2')?.textContent).toBe(copy.settingsModels)
  const blocks = Array.from(page.children).slice(1)
  expect(blocks.map(block => block.querySelector('h3, summary > span')?.textContent))
    .toEqual([copy.settingsDefaultModel, copy.settingsGlobalPrompt, copy.settingsContextBudget, copy.strataTitle])
  expect(page.querySelector<HTMLDetailsElement>('details[data-rainy-strata]')?.open).toBe(false)
})

it('saves the global prompt within the configured limit and refreshes the budget estimate', async () => {
  const { props } = fixture()
  render(<ModelsSection {...props} />)
  await waitFor(() => { expect(props.previewBudget).toHaveBeenCalledOnce() })
  const prompt = screen.getByLabelText<HTMLTextAreaElement>(zh.settingsGlobalPrompt)
  const save = () => screen.getByRole<HTMLButtonElement>('button', { name: zh.settingsSave })
  expect(prompt.maxLength).toBe(40)
  expect(save().disabled).toBe(true)
  fireEvent.change(prompt, { target: { value: '  Answer in Chinese.  ' } })
  expect(screen.getByText('22 / 40 字')).toBeTruthy()
  fireEvent.click(save())
  await waitFor(() => { expect(props.saveGlobalPrompt).toHaveBeenCalledExactlyOnceWith('  Answer in Chinese.  ') })
  await waitFor(() => { expect(prompt.value).toBe('Answer in Chinese.') })
  expect(save().disabled).toBe(true)
  expect(props.notify).toHaveBeenCalledWith(zh.settingsSaved, true)
  await waitFor(() => { expect(props.previewBudget).toHaveBeenCalledTimes(2) })
})

it('keeps an unsaved global prompt draft when the Host rejects it', async () => {
  const { props } = fixture()
  vi.mocked(props.saveGlobalPrompt).mockRejectedValueOnce(new Error('全局提示词超过 40 字符上限'))
  render(<ModelsSection {...props} />)
  const prompt = screen.getByLabelText<HTMLTextAreaElement>(zh.settingsGlobalPrompt)
  fireEvent.change(prompt, { target: { value: 'Draft instructions' } })
  fireEvent.click(screen.getByRole('button', { name: zh.settingsSave }))
  await waitFor(() => { expect(props.notify).toHaveBeenCalledWith('全局提示词超过 40 字符上限') })
  expect(prompt.value).toBe('Draft instructions')
  expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.settingsSave }).disabled).toBe(false)
})

it('keeps memory use and automatic generation independent and submits the edited revision', async () => {
  const { props } = fixture()
  props.memory = vi.fn(async () => ({ ...emptyMemory(), items: [{ id: 'note', text: 'Use the project interpreter.', sources: [] }] }))
  render(<MemorySection {...props} />)
  await screen.findByText('Use the project interpreter.')
  fireEvent.click(screen.getByLabelText(zh.settingsMemoryUse))
  await waitFor(() => { expect(props.memoryEnabled).toHaveBeenCalledWith('a', { enabled: false }) })
  await waitFor(() => { expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.settingsMemoryEdit }).disabled).toBe(false) })
  fireEvent.click(screen.getByRole('button', { name: zh.settingsMemoryEdit }))
  fireEvent.change(screen.getByLabelText(zh.settingsMemoryText), { target: { value: 'Use Python from the selected environment.' } })
  fireEvent.click(screen.getByRole('button', { name: zh.settingsSave }))
  await waitFor(() => { expect(props.memoryEdit).toHaveBeenCalledWith('a', 'note', 'Use Python from the selected environment.', 4) })
})

it('does not publish a delayed memory response after another project becomes selected', async () => {
  const { props, ide } = fixture()
  const pending = Promise.withResolvers<ProjectMemoryStatus>()
  props.memory = vi.fn(id => id === 'a' ? pending.promise : Promise.resolve({ ...emptyMemory(), items: [{ id: 'b', text: 'Project B fact', sources: [] }] }))
  render(<MemorySection {...props} />)
  await act(async () => { ide.state.set({ ...ide.state.getSnapshot(), workspace: { workspaceId: 'b' as WorkspaceId, path: '/project-b', title: 'B' } }) })
  await screen.findByText('Project B fact')
  await act(async () => { pending.resolve({ ...emptyMemory(), items: [{ id: 'a', text: 'Project A fact', sources: [] }] }); await pending.promise })
  expect(screen.queryByText('Project A fact')).toBeNull()
  expect(screen.getByText('Project B fact')).toBeTruthy()
})

it('shows component preparation progress and refreshes targets without changing the selected project', async () => {
  const { props, ide } = fixture()
  const pending = Promise.withResolvers<undefined>()
  const stop = vi.fn()
  let progress: ((message: string) => void) | undefined
  const windows = { id: 'windows-local', kind: 'windows' as const, label: 'Windows' }
  const ubuntu = { id: 'wsl:Ubuntu', kind: 'wsl' as const, label: 'Ubuntu' }
  const targets = vi.fn().mockResolvedValueOnce({ current: windows, targets: [windows] })
    .mockResolvedValueOnce({ current: windows, targets: [windows, ubuntu] })
  const switchTarget = vi.fn(async () => ({ ok: true }))
  const snapshot: RuntimeSnapshot = { targetId: windows.id as RuntimeSnapshot['targetId'], workspaceId: 'a' as WorkspaceId,
    platform: 'windows', candidates: [], selected: {} }
  props.runtime = vi.fn<SettingsSectionProps['runtime']>(async () => snapshot)
  const runtimeNative: NonNullable<SettingsSectionProps['runtimeNative']> = { targets, prepare: () => pending.promise, switchTarget,
    onProgress: (listener: (message: string) => void) => { progress = listener; return stop } }
  const page = render(<RuntimeSection {...props} runtimeNative={runtimeNative} />)
  await screen.findByRole('button', { name: zh.settingsExecutionTarget })
  fireEvent.click(screen.getByRole('button', { name: zh.settingsPrepareEnvironments }))
  await screen.findByText(zh.settingsPreparingEnvironments)
  act(() => { progress?.('正在校验离线组件 2 / 3') })
  expect(screen.getByText('正在校验离线组件 2 / 3')).toBeTruthy()
  await act(async () => { pending.resolve(undefined); await pending.promise })
  await waitFor(() => { expect(targets).toHaveBeenCalledTimes(2) })
  expect(screen.queryByText('正在校验离线组件 2 / 3')).toBeNull()
  expect(ide.state.getSnapshot().workspace?.workspaceId).toBe('a')
  fireEvent.click(screen.getByRole('button', { name: zh.settingsExecutionTarget }))
  fireEvent.click(await screen.findByRole('menuitem', { name: 'Ubuntu' }))
  await waitFor(() => { expect(switchTarget).toHaveBeenCalledWith({ targetId: 'wsl:Ubuntu', workspaceId: 'a' }) })
  page.unmount()
  expect(stop).toHaveBeenCalledOnce()
})

it('clears the waiting message and reports a failed native component preparation', async () => {
  const { props } = fixture()
  const windows = { id: 'windows-local', kind: 'windows' as const, label: 'Windows' }
  props.runtime = vi.fn<SettingsSectionProps['runtime']>(async () => ({ targetId: windows.id as RuntimeSnapshot['targetId'], workspaceId: 'a' as WorkspaceId,
    platform: 'windows', candidates: [], selected: {} }))
  const runtimeNative: NonNullable<SettingsSectionProps['runtimeNative']> = { targets: vi.fn(async () => ({ current: windows, targets: [windows] })),
    prepare: vi.fn(async () => { throw new Error('离线包校验失败') }), switchTarget: vi.fn(), onProgress: () => vi.fn() }
  render(<RuntimeSection {...props} runtimeNative={runtimeNative} />)
  fireEvent.click(screen.getByRole('button', { name: zh.settingsPrepareEnvironments }))
  await waitFor(() => { expect(props.notify).toHaveBeenCalledWith('离线包校验失败') })
  expect(screen.queryByText(zh.settingsPreparingEnvironments)).toBeNull()
  expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.settingsPrepareEnvironments }).disabled).toBe(false)
})
