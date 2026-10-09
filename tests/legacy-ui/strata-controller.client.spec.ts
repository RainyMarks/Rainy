/** Native results are serialized and cannot overwrite a newer local-model action. */
import { afterEach, expect, it } from 'vitest'
import { StrataController } from '../src/client/strata-controller.ts'
import type { StrataStatus } from '../src/strata-protocol.ts'
import { strataFixture } from './strata-fixture.client.ts'

const controllers: StrataController[] = []
afterEach(() => { for (const controller of controllers.splice(0)) controller.dispose() })
function fixture() { const h = strataFixture(); controllers.push(h.controller); return h }

it('coalesces reads without starting or connecting a model and retains status on failure', async () => {
  const h = fixture()
  const pending = Promise.withResolvers<StrataStatus>()
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
  expect(h.controller.state.getSnapshot()).toMatchObject({ status: h.status, loading: false, error: 'Desktop disconnected' })
})

it('does not let an older poll replace newly saved and inspected settings', async () => {
  const h = fixture()
  const earlier = Promise.withResolvers<StrataStatus>()
  h.bridge.status.mockReturnValueOnce(earlier.promise)
  const poll = h.controller.refresh()
  const settings = { ...h.status.settings, modelPath: 'C:\\models\\selected-profile.json', contextWindow: 131072 }
  await h.controller.actions.strataSave(settings)
  earlier.resolve(h.status)
  await poll
  expect(h.controller.state.getSnapshot().status?.settings).toEqual(settings)
  expect(h.notify).toHaveBeenCalledWith('Strata saved', true)
})

it('allows stop to cancel a pending start and ignores its late completion', async () => {
  const h = fixture()
  await h.controller.refresh()
  const pending = Promise.withResolvers<StrataStatus>()
  h.bridge.start.mockReturnValueOnce(pending.promise)
  const start = h.controller.actions.strataStart()
  await h.controller.actions.strataStart()
  await h.controller.actions.strataStop()
  pending.resolve({ ...h.status, phase: 'starting' })
  await start
  expect(h.bridge.start).toHaveBeenCalledOnce()
  expect(h.bridge.stop).toHaveBeenCalledOnce()
  expect(h.controller.state.getSnapshot()).toMatchObject({ pending: undefined, status: { phase: 'stopped' } })
  expect(h.notify).toHaveBeenCalledExactlyOnceWith('Strata stopped', true)
})

it('treats a cancelled native picker as no change and admits only one picker', async () => {
  const h = fixture()
  const pending = Promise.withResolvers<string | null>()
  h.bridge.selectModel.mockReturnValueOnce(pending.promise)
  const choosing = h.controller.actions.strataChoose('gguf')
  expect(await h.controller.actions.strataChoose('profile')).toBeNull()
  pending.resolve(null)
  expect(await choosing).toBeNull()
  expect(h.bridge.selectModel).toHaveBeenCalledExactlyOnceWith('gguf')
  expect(h.bridge.save).not.toHaveBeenCalled()
  expect(h.notify).not.toHaveBeenCalled()
})

it('loads the actual saved model only after the selected Host connection succeeds', async () => {
  const h = fixture()
  expect(await h.controller.actions.strataConnect()).toEqual(h.model)
  expect(h.bridge.connect).toHaveBeenCalledOnce()
  expect(h.configured).toHaveBeenCalledExactlyOnceWith({ provider: 'rainy-strata', model: 'served-model' })
  expect(h.notify).toHaveBeenCalledWith('Strata connected', true)
  h.bridge.connect.mockRejectedValueOnce(new Error('Switch to Windows to reach the local Strata service.'))
  expect(await h.controller.actions.strataConnect()).toBeUndefined()
  expect(h.configured).toHaveBeenCalledOnce()
  expect(h.notify).toHaveBeenLastCalledWith('Switch to Windows to reach the local Strata service.')
})

it('retains the previous model and reports save or startup failures', async () => {
  const h = fixture()
  await h.controller.refresh()
  h.bridge.save.mockRejectedValueOnce(new Error('Unsupported model architecture'))
  expect(await h.controller.actions.strataSave({ ...h.status.settings, modelPath: 'C:\\models\\other.gguf' })).toBeUndefined()
  expect(h.controller.state.getSnapshot().status).toEqual(h.status)
  h.bridge.start.mockRejectedValueOnce(new Error('Matching MTP weights are missing'))
  await h.controller.actions.strataStart()
  expect(h.notify).toHaveBeenLastCalledWith('Matching MTP weights are missing')
  expect(h.bridge.connect).not.toHaveBeenCalled()
})

it('suppresses status and connection completion after disposal', async () => {
  const h = fixture()
  const status = Promise.withResolvers<StrataStatus>()
  h.bridge.status.mockReturnValueOnce(status.promise)
  const reading = h.controller.refresh()
  h.controller.dispose()
  const frozen = h.controller.state.getSnapshot()
  status.resolve(h.status)
  await reading
  expect(h.controller.state.getSnapshot()).toBe(frozen)

  const other = fixture()
  const selection = Promise.withResolvers<{ provider: string; model: string }>()
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
  expect(controller.state.getSnapshot()).toEqual({ available: false, loading: false, error: '' })
  expect(h.notify).not.toHaveBeenCalled()
})
