// @vitest-environment jsdom
/** Strata settings expose user-owned weights and explicit process and connection actions. */
import { afterEach, expect, it } from 'vitest'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { bindSnapshotSelector } from '@deepseek-ai/dsh-client-test-runtime'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import { StrataSettings } from '../src/client/StrataSettings.tsx'
import type { StrataController } from '../src/client/strata-controller.ts'
import { en, zh } from '../src/client/locales.ts'
import { strataFixture } from './strata-fixture.client.ts'

const controllers: StrataController[] = []
afterEach(() => { cleanup(); for (const controller of controllers.splice(0)) controller.dispose() })

function fixture(locale: 'zh' | 'en' = 'zh') {
  const h = strataFixture()
  controllers.push(h.controller)
  const copy = locale === 'zh' ? zh : en
  const t = ((key: keyof typeof zh, values?: Record<string, string | number>) =>
    copy[key].replace(/\{(\w+)\}/g, (match, name: string) => values?.[name] === undefined ? match : String(values[name]))) as TranslateNS<'rainy'>
  const useStrata = bindSnapshotSelector(h.controller.state)
  function Page() { return <StrataSettings snapshot={useStrata(value => value)} t={t} {...h.controller.actions} /> }
  return { ...h, copy, show: () => render(<Page />), mount: async () => { await h.controller.refresh(); return render(<Page />) } }
}

it.each(['zh', 'en'] as const)('shows bundled runtime and required MTP weights without request settings in %s', async (locale) => {
  const h = fixture(locale)
  const { container } = await h.mount()
  expect(screen.getByText(h.copy.strataSupported)).toBeTruthy()
  expect(screen.queryByLabelText(h.copy.settingsThinking)).toBeNull()
  expect(screen.queryByLabelText(h.copy.settingsOutput)).toBeNull()
  const card = container.querySelector('[data-rainy-strata]')!
  const copy = Array.from(card.querySelectorAll('*')).filter(value => value.matches('summary > span:first-child, p, [role="status"]'))
    .map(value => value.textContent).filter(Boolean)
  const fields = Array.from(card.querySelectorAll('label')).map((label) => {
    const input = label.querySelector('input')
    return `${label.querySelector('span')?.textContent}: ${input?.value ?? label.querySelector('button')?.textContent ?? ''}`.trimEnd()
  })
  const controls = Array.from(card.querySelectorAll('button')).map(button => `${button.textContent}: ${button.disabled ? 'disabled' : 'enabled'}`)
  await expect([...copy, ...fields, ...controls].join('\n') + '\n').toMatchFileSnapshot(`./expected/strata-${locale}.txt`)
})

it('starts collapsed with the engine phase in its summary and expands on request', async () => {
  const h = fixture()
  const { container } = await h.mount()
  const card = container.querySelector<HTMLDetailsElement>('details[data-rainy-strata]')!
  expect(card.open).toBe(false)
  expect(card.querySelector('summary')?.textContent).toBe(`${zh.strataTitle}${zh.strataStoppedState}`)
  fireEvent.click(card.querySelector('summary')!)
  await waitFor(() => { expect(card.open).toBe(true) })
  await act(async () => { await h.controller.refresh() })
  expect(card.open).toBe(true)
})

it('keeps a cancelled picker unchanged and saves selected main and MTP files before startup', async () => {
  const h = fixture()
  await h.mount()
  fireEvent.click(screen.getByRole('button', { name: zh.strataChooseGguf }))
  await waitFor(() => { expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.strataChooseGguf }).disabled).toBe(false) })
  expect(screen.getByLabelText<HTMLInputElement>(zh.strataModelPath).value).toBe(h.status.settings.modelPath)
  expect(h.bridge.save).not.toHaveBeenCalled()
  h.bridge.selectModel.mockResolvedValueOnce('C:\\models\\selected-00001-of-00003.gguf')
  fireEvent.click(screen.getByRole('button', { name: zh.strataChooseGguf }))
  await waitFor(() => { expect(screen.getByLabelText<HTMLInputElement>(zh.strataModelPath).value).toBe('C:\\models\\selected-00001-of-00003.gguf') })
  h.bridge.selectModel.mockResolvedValueOnce('C:\\models\\matching-mtp.gguf')
  fireEvent.click(screen.getByRole('button', { name: zh.strataChooseMtpFile }))
  await waitFor(() => { expect(screen.getByLabelText<HTMLInputElement>(zh.strataMtpPath).value).toBe('C:\\models\\matching-mtp.gguf') })
  expect(h.bridge.selectModel).toHaveBeenLastCalledWith('mtp')
  expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.strataStart }).disabled).toBe(true)
  fireEvent.click(screen.getByRole('button', { name: zh.strataSave }))
  await waitFor(() => { expect(h.bridge.save).toHaveBeenCalledOnce() })
  expect(h.bridge.save.mock.calls[0]?.[0]).toEqual({ ...h.status.settings,
    modelPath: 'C:\\models\\selected-00001-of-00003.gguf', mtpPath: 'C:\\models\\matching-mtp.gguf' })
  await waitFor(() => { expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.strataStart }).disabled).toBe(false) })
  expect(h.bridge.start).not.toHaveBeenCalled()
})

it('adopts normalized profile settings while ordinary status refresh preserves unsaved edits', async () => {
  const h = fixture()
  await h.mount()
  fireEvent.change(screen.getByLabelText(zh.strataPort), { target: { value: '9090' } })
  await act(async () => { await h.controller.refresh() })
  expect(screen.getByLabelText<HTMLInputElement>(zh.strataPort).value).toBe('9090')
  h.bridge.selectModel.mockResolvedValueOnce('C:\\models\\profile.json')
  fireEvent.click(screen.getByRole('button', { name: zh.strataChooseProfile }))
  await waitFor(() => { expect(screen.getByLabelText<HTMLInputElement>(zh.strataModelPath).value).toBe('C:\\models\\profile.json') })
  h.bridge.save.mockResolvedValueOnce({ ...h.status, settings: { ...h.status.settings,
    modelPath: 'C:\\models\\profile.json', port: 8082, contextWindow: 65536, residentBudgetGiB: 39 } })
  fireEvent.click(screen.getByRole('button', { name: zh.strataSave }))
  await waitFor(() => { expect(screen.getByLabelText<HTMLInputElement>(zh.strataPort).value).toBe('8082') })
  expect(screen.getByRole('button', { name: zh.strataContext }).textContent).toContain('65,536')
  expect(screen.getByLabelText<HTMLInputElement>(zh.strataResidentBudget).value).toBe('39')
})

it('offers cancellation during local preparation without connecting a model', async () => {
  const h = fixture()
  await h.mount()
  fireEvent.click(screen.getByRole('button', { name: zh.strataStart }))
  await screen.findByText(zh.strataPreparing)
  expect(screen.getByLabelText<HTMLInputElement>(zh.strataPort).disabled).toBe(true)
  expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.strataConnect }).disabled).toBe(true)
  fireEvent.click(screen.getByRole('button', { name: zh.strataCancelStart }))
  await screen.findByText(zh.strataStoppedState)
  expect(h.bridge.stop).toHaveBeenCalledOnce()
  expect(h.bridge.connect).not.toHaveBeenCalled()
})

it('can connect a loaded external service but cannot stop it or supply hidden credentials', async () => {
  const h = fixture()
  h.bridge.status.mockResolvedValue({ ...h.status, phase: 'external',
    server: { baseURL: 'http://127.0.0.1:8081/v1', model: 'health-model', contextWindow: 65536,
      loaded: true, owned: false, authenticationRequired: false } })
  await h.mount()
  expect(screen.getByText('运行模型：health-model · 实际窗口：65,536 tokens')).toBeTruthy()
  expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.strataStop }).disabled).toBe(true)
  fireEvent.click(screen.getByRole('button', { name: zh.strataConnect }))
  await waitFor(() => { expect(h.bridge.connect).toHaveBeenCalledOnce() })
  await waitFor(() => { expect(h.controller.state.getSnapshot().pending).toBeUndefined() })
  h.bridge.status.mockResolvedValue({ ...h.status, phase: 'external',
    server: { baseURL: 'http://127.0.0.1:8081/v1', model: 'health-model', contextWindow: 65536,
      loaded: true, owned: false, authenticationRequired: true } })
  await act(async () => { await h.controller.refresh() })
  expect(screen.getByText(zh.strataAuthentication)).toBeTruthy()
  expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.strataConnect }).disabled).toBe(true)
  expect(h.bridge.stop).not.toHaveBeenCalled()
})

it('shows a missing bundled runtime and rejects invalid ports without a native save', async () => {
  const h = fixture()
  h.bridge.status.mockResolvedValue({ ...h.status, runtime: { ...h.status.runtime, available: false, missing: ['python.exe'] } })
  await h.mount()
  expect(screen.getByText(zh.strataRuntimeMissing)).toBeTruthy()
  expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.strataStart }).disabled).toBe(true)
  fireEvent.change(screen.getByLabelText(zh.strataPort), { target: { value: '80' } })
  expect(screen.getByRole('alert').textContent).toBe(zh.strataInvalid)
  expect(screen.getByRole<HTMLButtonElement>('button', { name: zh.strataSave }).disabled).toBe(true)
  expect(h.bridge.save).not.toHaveBeenCalled()
})

it('keeps read failures retryable without displaying an empty configuration form', async () => {
  const h = fixture()
  h.bridge.status.mockRejectedValueOnce(new Error('Native status unavailable'))
  await h.mount()
  expect(screen.getByRole('alert').textContent).toBe('Native status unavailable')
  expect(screen.queryByLabelText(zh.strataModelPath)).toBeNull()
  fireEvent.click(screen.getByRole('button', { name: zh.settingsRetry }))
  await screen.findByLabelText(zh.strataModelPath)
  expect(screen.queryByRole('alert')).toBeNull()
})
