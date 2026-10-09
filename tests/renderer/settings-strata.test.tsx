// @vitest-environment happy-dom
/** The Strata card exposes user-owned weights and explicit engine and connection actions; native results never overwrite newer actions. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import { act } from 'react'
import type { ModelSetup } from '../../src/shared/rpc.ts'
import type { StrataStatus } from '../../src/shared/strata-protocol.ts'
import { StrataSettings } from '../../src/renderer/settings/StrataSettings.tsx'
import { StrataController, useStrata } from '../../src/renderer/settings/strata-controller.ts'
import { settingsMessages } from '../../src/renderer/settings/messages.ts'
import { all, button, cleanup, click, control, deferred, emit, hasText, render, strataBridge, strataStatus, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const zh = settingsMessages.zh
const en = settingsMessages.en
const controllers: StrataController[] = []

beforeEach(async () => {
  await emit('prefs.changed', { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true })
})
afterEach(async () => { await cleanup(); for (const controller of controllers.splice(0)) controller.dispose() })

function fixture() {
  const status = strataStatus()
  const bridge = strataBridge(status)
  const model: ModelSetup = { provider: 'rainy-strata', model: 'served-model', baseURL: 'http://127.0.0.1:8081/v1',
    contextWindow: 65536, api: 'openai-completions', local: true, thinking: 'off', thinkingFormat: 'openai', maxTokensField: 'max_tokens' }
  const configured = vi.fn(async () => model)
  const notify = vi.fn()
  const copy = { saved: () => 'Strata saved', stopped: () => 'Strata stopped', connected: () => 'Strata connected' }
  const controller = new StrataController(bridge, copy, notify, configured)
  controllers.push(controller)
  function Card() { return <StrataSettings snapshot={useStrata(controller)} actions={controller.actions} /> }
  return { status, bridge, model, configured, notify, copy, controller,
    mount: async () => { await controller.refresh(); return render(<Card />) } }
}

it.each([{ locale: 'zh' as const, copy: zh }, { locale: 'en' as const, copy: en }])('shows the bundled engine and required MTP weights without request settings in $locale', async ({ locale, copy }) => {
  await emit('prefs.changed', { locale, theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true })
  const h = fixture()
  const { container } = await h.mount()
  expect(hasText(copy.strataSupported)).toBe(true)
  expect(all(`[aria-label="${copy.settingsThinking}"]`)).toEqual([])
  const card = container.querySelector('[data-strata]')!
  const text = all('summary > span:first-child, p, [role="status"]', card).map(element => element.textContent).filter(Boolean)
  const fields = all('label', card).map(label => `${label.querySelector('span')?.textContent}: ${label.querySelector('input')?.value ?? label.querySelector('button')?.textContent ?? ''}`.trimEnd())
  const controls = all<HTMLButtonElement>('button', card).map(element => `${element.textContent}: ${element.disabled ? 'disabled' : 'enabled'}`)
  expect([...text, ...fields, ...controls]).toEqual(locale === 'zh' ? [
    'Strata 本地模型', '已停止', 'Strata 引擎已就绪 · 0.1.39',
    '支持 Qwen3.8 Flash Next GGUF 与配套 MTP 权重；分片模型请选择首片，也可导入 Strata profile',
    '首次启动会在本机准备运行文件，模型权重不会自动下载',
    '模型文件、模型目录或 profile: C:\\models\\main.gguf', 'MTP 文件或目录（留空自动检测）: C:\\models\\mtp.gguf',
    '引擎上下文长度: 32,768', '本地端口: 8081', 'KV 缓存: int8', '保留显存（MiB）: 700', '常驻内存预算（GiB，留空自动）:',
    '刷新: enabled', '选择 GGUF: enabled', '选择模型目录: enabled', '导入 profile: enabled', '选择 MTP 文件: enabled', '选择 MTP 目录: enabled',
    '32,768: enabled', 'int8: enabled', '保存 Strata 配置: disabled', '启动本地模型: enabled', '停止本地模型: disabled', '连接并设为默认: disabled',
  ] : [
    'Strata local model', 'Stopped', 'The Strata engine is ready · 0.1.39',
    'Supports Qwen3.8 Flash Next GGUF with matching MTP weights. Select the first shard of a split model, or import a Strata profile.',
    'The first start prepares runtime files locally. Model weights are never downloaded automatically.',
    'Model file, model directory, or profile: C:\\models\\main.gguf', 'MTP file or directory (automatic detection when empty): C:\\models\\mtp.gguf',
    'Engine context window: 32,768', 'Local port: 8081', 'KV cache: int8', 'VRAM reserve (MiB): 700', 'Resident RAM budget (GiB, automatic when empty):',
    'Refresh: enabled', 'Select GGUF: enabled', 'Select model directory: enabled', 'Import profile: enabled', 'Select MTP file: enabled',
    'Select MTP directory: enabled', '32,768: enabled', 'int8: enabled', 'Save Strata configuration: disabled', 'Start local model: enabled',
    'Stop local model: disabled', 'Connect and use by default: disabled',
  ])
})

it('starts collapsed with the engine phase in its summary', async () => {
  const h = fixture()
  const { container } = await h.mount()
  const card = container.querySelector<HTMLDetailsElement>('details[data-strata]')!
  expect(card.open).toBe(false)
  expect(card.querySelector('summary')?.textContent).toBe(`${zh.strataTitle}${zh.strataStoppedState}`)
})

it('keeps a cancelled picker unchanged and saves selected main and MTP files before startup', async () => {
  const h = fixture()
  await h.mount()
  await click(button(zh.strataChooseGguf))
  expect(control<HTMLInputElement>(zh.strataModelPath).value).toBe(h.status.settings.modelPath)
  expect(h.bridge.save).not.toHaveBeenCalled()
  h.bridge.selectModel.mockResolvedValueOnce('C:\\models\\selected-00001-of-00003.gguf')
  await click(button(zh.strataChooseGguf))
  await waitFor(() => { expect(control<HTMLInputElement>(zh.strataModelPath).value).toBe('C:\\models\\selected-00001-of-00003.gguf') })
  h.bridge.selectModel.mockResolvedValueOnce('C:\\models\\matching-mtp.gguf')
  await click(button(zh.strataChooseMtpFile))
  await waitFor(() => { expect(control<HTMLInputElement>(zh.strataMtpPath).value).toBe('C:\\models\\matching-mtp.gguf') })
  expect(h.bridge.selectModel).toHaveBeenLastCalledWith('mtp')
  expect(button(zh.strataStart).disabled).toBe(true)
  await click(button(zh.strataSave))
  await waitFor(() => { expect(h.bridge.save).toHaveBeenCalledOnce() })
  expect(h.bridge.save.mock.calls[0]?.[0]).toEqual({ ...h.status.settings,
    modelPath: 'C:\\models\\selected-00001-of-00003.gguf', mtpPath: 'C:\\models\\matching-mtp.gguf' })
  await waitFor(() => { expect(button(zh.strataStart).disabled).toBe(false) })
  expect(h.bridge.start).not.toHaveBeenCalled()
})

it('adopts normalized profile settings while an ordinary status read preserves unsaved edits', async () => {
  const h = fixture()
  await h.mount()
  await type(control(zh.strataPort), '9090')
  await act(async () => { await h.controller.refresh() })
  expect(control<HTMLInputElement>(zh.strataPort).value).toBe('9090')
  h.bridge.selectModel.mockResolvedValueOnce('C:\\models\\profile.json')
  await click(button(zh.strataChooseProfile))
  await waitFor(() => { expect(control<HTMLInputElement>(zh.strataModelPath).value).toBe('C:\\models\\profile.json') })
  h.bridge.save.mockResolvedValueOnce({ ...h.status, settings: { ...h.status.settings,
    modelPath: 'C:\\models\\profile.json', port: 8082, contextWindow: 65536, residentBudgetGiB: 39 } })
  await click(button(zh.strataSave))
  await waitFor(() => { expect(control<HTMLInputElement>(zh.strataPort).value).toBe('8082') })
  expect(button(zh.strataContext).textContent).toContain('65,536')
  expect(control<HTMLInputElement>(zh.strataResidentBudget).value).toBe('39')
})

it('offers cancellation during local preparation without connecting a model', async () => {
  const h = fixture()
  await h.mount()
  await click(button(zh.strataStart))
  await waitFor(() => { expect(hasText(zh.strataPreparing)).toBe(true) })
  expect(control<HTMLInputElement>(zh.strataPort).disabled).toBe(true)
  expect(button(zh.strataConnect).disabled).toBe(true)
  await click(button(zh.strataCancelStart))
  await waitFor(() => { expect(hasText(zh.strataStoppedState)).toBe(true) })
  expect(h.bridge.stop).toHaveBeenCalledOnce()
  expect(h.bridge.connect).not.toHaveBeenCalled()
})

it('can connect a loaded external service but cannot stop it or supply hidden credentials', async () => {
  const h = fixture()
  const external = (authenticationRequired: boolean): StrataStatus => ({ ...h.status, phase: 'external',
    server: { baseURL: 'http://127.0.0.1:8081/v1', model: 'health-model', contextWindow: 65536, loaded: true, owned: false, authenticationRequired } })
  h.bridge.status.mockResolvedValue(external(false))
  await h.mount()
  expect(hasText('运行模型：health-model · 实际窗口：65,536 tokens')).toBe(true)
  expect(button(zh.strataStop).disabled).toBe(true)
  await click(button(zh.strataConnect))
  await waitFor(() => { expect(h.bridge.connect).toHaveBeenCalledOnce() })
  await waitFor(() => { expect(h.controller.getSnapshot().pending).toBeUndefined() })
  h.bridge.status.mockResolvedValue(external(true))
  await act(async () => { await h.controller.refresh() })
  expect(hasText(zh.strataAuthentication)).toBe(true)
  expect(button(zh.strataConnect).disabled).toBe(true)
  expect(h.bridge.stop).not.toHaveBeenCalled()
})

it('shows a missing engine and rejects invalid ports without a native save', async () => {
  const h = fixture()
  h.bridge.status.mockResolvedValue({ ...h.status, runtime: { ...h.status.runtime, available: false, missing: ['python.exe'] } })
  await h.mount()
  expect(hasText(zh.strataRuntimeMissing)).toBe(true)
  expect(button(zh.strataStart).disabled).toBe(true)
  await type(control(zh.strataPort), '80')
  expect(document.querySelector('[role="alert"]')?.textContent).toBe(zh.strataInvalid)
  expect(button(zh.strataSave).disabled).toBe(true)
  expect(h.bridge.save).not.toHaveBeenCalled()
})

it('keeps read failures retryable without showing an empty configuration form', async () => {
  const h = fixture()
  h.bridge.status.mockRejectedValueOnce(new Error('Native status unavailable'))
  await h.mount()
  expect(document.querySelector('[role="alert"]')?.textContent).toBe('Native status unavailable')
  expect(all(`[aria-label="${zh.strataModelPath}"]`)).toEqual([])
  await click(button(zh.settingsRetry))
  await waitFor(() => { expect(control(zh.strataModelPath)).toBeDefined() })
  expect(document.querySelector('[role="alert"]')).toBeNull()
})

it('tells a browser page to manage Strata in the desktop app', async () => {
  const controller = new StrataController(undefined, { saved: () => '', stopped: () => '', connected: () => '' }, vi.fn(), vi.fn())
  controllers.push(controller)
  function Card() { return <StrataSettings snapshot={useStrata(controller)} actions={controller.actions} /> }
  await render(<Card />)
  expect(hasText(zh.strataDesktopOnly)).toBe(true)
})

it('coalesces reads without starting or connecting a model and keeps the status after a failed read', async () => {
  const h = fixture()
  const pending = deferred<StrataStatus>()
  h.bridge.status.mockReturnValueOnce(pending.promise)
  const first = h.controller.refresh()
  expect(h.controller.refresh()).toBe(first)
  pending.resolve(h.status)
  await first
  expect(h.bridge.status).toHaveBeenCalledOnce()
  expect(h.bridge.start).not.toHaveBeenCalled()
  expect(h.bridge.connect).not.toHaveBeenCalled()
  expect(h.bridge.save).not.toHaveBeenCalled()
  h.bridge.status.mockRejectedValueOnce(new Error('Desktop disconnected'))
  await h.controller.refresh()
  expect(h.controller.getSnapshot()).toMatchObject({ status: h.status, loading: false, error: 'Desktop disconnected' })
})

it('does not let an older read replace newly saved settings', async () => {
  const h = fixture()
  const earlier = deferred<StrataStatus>()
  h.bridge.status.mockReturnValueOnce(earlier.promise)
  const poll = h.controller.refresh()
  const settings = { ...h.status.settings, modelPath: 'C:\\models\\selected-profile.json', contextWindow: 131072 }
  await h.controller.actions.strataSave(settings)
  earlier.resolve(h.status)
  await poll
  expect(h.controller.getSnapshot().status?.settings).toEqual(settings)
  expect(h.notify).toHaveBeenCalledWith('Strata saved', true)
})

it('lets stop cancel a pending start and ignores its late completion', async () => {
  const h = fixture()
  await h.controller.refresh()
  const pending = deferred<StrataStatus>()
  h.bridge.start.mockReturnValueOnce(pending.promise)
  const start = h.controller.actions.strataStart()
  await h.controller.actions.strataStart()
  await h.controller.actions.strataStop()
  pending.resolve({ ...h.status, phase: 'starting' })
  await start
  expect(h.bridge.start).toHaveBeenCalledOnce()
  expect(h.bridge.stop).toHaveBeenCalledOnce()
  expect(h.controller.getSnapshot()).toMatchObject({ pending: undefined, status: { phase: 'stopped' } })
  expect(h.notify).toHaveBeenCalledExactlyOnceWith('Strata stopped', true)
})

it('treats a cancelled picker as no change and admits one picker at a time', async () => {
  const h = fixture()
  const pending = deferred<string | null>()
  h.bridge.selectModel.mockReturnValueOnce(pending.promise)
  const choosing = h.controller.actions.strataChoose('gguf')
  expect(await h.controller.actions.strataChoose('profile')).toBeNull()
  pending.resolve(null)
  expect(await choosing).toBeNull()
  expect(h.bridge.selectModel).toHaveBeenCalledExactlyOnceWith('gguf')
  expect(h.notify).not.toHaveBeenCalled()
})

it('reads the saved model only after the desktop connection succeeds', async () => {
  const h = fixture()
  expect(await h.controller.actions.strataConnect()).toEqual(h.model)
  expect(h.configured).toHaveBeenCalledExactlyOnceWith({ provider: 'rainy-strata', model: 'served-model' })
  expect(h.notify).toHaveBeenCalledWith('Strata connected', true)
  h.bridge.connect.mockRejectedValueOnce(new Error('Switch to Windows to reach the local Strata service.'))
  expect(await h.controller.actions.strataConnect()).toBeUndefined()
  expect(h.configured).toHaveBeenCalledOnce()
  expect(h.notify).toHaveBeenLastCalledWith('Switch to Windows to reach the local Strata service.')
})

it('keeps the previous status and reports save or startup failures', async () => {
  const h = fixture()
  await h.controller.refresh()
  h.bridge.save.mockRejectedValueOnce(new Error('Unsupported model architecture'))
  expect(await h.controller.actions.strataSave({ ...h.status.settings, modelPath: 'C:\\models\\other.gguf' })).toBeUndefined()
  expect(h.controller.getSnapshot().status).toEqual(h.status)
  h.bridge.start.mockRejectedValueOnce(new Error('Matching MTP weights are missing'))
  await h.controller.actions.strataStart()
  expect(h.notify).toHaveBeenLastCalledWith('Matching MTP weights are missing')
  expect(h.bridge.connect).not.toHaveBeenCalled()
})

it('ignores status and connection completions after disposal', async () => {
  const h = fixture()
  const status = deferred<StrataStatus>()
  h.bridge.status.mockReturnValueOnce(status.promise)
  const reading = h.controller.refresh()
  h.controller.dispose()
  const frozen = h.controller.getSnapshot()
  status.resolve(h.status)
  await reading
  expect(h.controller.getSnapshot()).toBe(frozen)

  const other = fixture()
  const selection = deferred<{ provider: string; model: string }>()
  other.bridge.connect.mockReturnValueOnce(selection.promise)
  const connecting = other.controller.actions.strataConnect()
  other.controller.dispose()
  selection.resolve({ provider: 'rainy-strata', model: 'served-model' })
  expect(await connecting).toBeUndefined()
  expect(other.configured).not.toHaveBeenCalled()
  expect(other.notify).not.toHaveBeenCalled()
})

it('has no native effects without the desktop bridge', async () => {
  const h = fixture()
  const controller = new StrataController(undefined, h.copy, h.notify, h.configured)
  controllers.push(controller)
  await controller.refresh()
  await controller.actions.strataStart()
  await controller.actions.strataConnect()
  expect(controller.getSnapshot()).toEqual({ available: false, loading: false, error: '' })
  expect(h.notify).not.toHaveBeenCalled()
})
