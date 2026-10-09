/** Updater transport and saved-shutdown barriers are isolated from Electron and external services. */
import { EventEmitter } from 'node:events'
import { afterEach, expect, it, vi } from 'vitest'
import { RainyUpdates } from '../src/updates.ts'
import type { RainyUpdater, RainyUpdateHooks, RainyUpdateState } from '../src/updates.ts'

const controllers: RainyUpdates[] = []
afterEach(() => { for (const controller of controllers.splice(0)) controller.dispose(); vi.restoreAllMocks() })

type CheckedUpdate = NonNullable<Awaited<ReturnType<RainyUpdater['checkForUpdates']>>>

function checked(available = true): CheckedUpdate {
  return { isUpdateAvailable: available, updateInfo: { version: '1.1.0' } }
}

function fixture(enabled = true) {
  const events = new EventEmitter()
  const checkForUpdates = vi.fn<RainyUpdater['checkForUpdates']>(async () => checked())
  const prepared = (version = '1.1.0'): void => {
    events.emit('update-downloaded', { ...checked().updateInfo, version, downloadedFile: 'prepared-installer.exe' })
  }
  const downloadUpdate = vi.fn<RainyUpdater['downloadUpdate']>(async () => {
    events.emit('download-progress', { percent: 42, bytesPerSecond: 512, transferred: 42, total: 100, delta: 42 })
    prepared()
    return ['prepared-installer.exe']
  })
  const quitAndInstall = vi.fn<RainyUpdater['quitAndInstall']>()
  const updater: RainyUpdater = Object.assign(events, {
    autoDownload: true, autoInstallOnAppQuit: true, allowPrerelease: true, allowDowngrade: true, channel: 'nightly',
    setFeedURL: vi.fn<RainyUpdater['setFeedURL']>(), checkForUpdates, downloadUpdate, quitAndInstall,
  })
  const states: { state: RainyUpdateState; manual: boolean }[] = []
  const hooks = {
    enabled,
    notice: vi.fn<RainyUpdateHooks['notice']>((state, manual) => { states.push({ state, manual }) }),
    confirm: vi.fn<RainyUpdateHooks['confirm']>(async () => false),
    restartWithInstall: vi.fn<RainyUpdateHooks['restartWithInstall']>(async (install) => { install(); return true }),
  }
  const controller = new RainyUpdates(updater, hooks)
  controllers.push(controller)
  return { controller, updater, events, prepared, hooks, states, checkForUpdates, downloadUpdate, quitAndInstall }
}

it('uses the public stable feed and never installs on an ordinary quit', async () => {
  const f = fixture()
  expect(f.updater.setFeedURL).toHaveBeenCalledWith({ provider: 'github', owner: 'RainyMarks', repo: 'RainyAgent' })
  expect(f.updater).toMatchObject({ autoDownload: false, autoInstallOnAppQuit: false,
    allowPrerelease: false, allowDowngrade: false, channel: 'latest' })
  await expect(f.controller.check()).resolves.toEqual({ phase: 'ready', version: '1.1.0' })
  expect(f.downloadUpdate).toHaveBeenCalledOnce()
  expect(f.hooks.confirm).toHaveBeenCalledExactlyOnceWith('1.1.0')
  expect(f.hooks.restartWithInstall).not.toHaveBeenCalled()
  expect(f.quitAndInstall).not.toHaveBeenCalled()
  expect(f.states.every(entry => !entry.manual)).toBe(true)
  expect(f.states.map(entry => entry.state.phase)).toEqual(['checking', 'downloading', 'downloading', 'ready'])
})

it('reports current and unavailable states for manual checks without downloading', async () => {
  const f = fixture()
  f.checkForUpdates.mockResolvedValueOnce(checked(false))
  await expect(f.controller.check(true)).resolves.toEqual({ phase: 'current' })
  expect(f.states).toEqual([{ state: { phase: 'checking' }, manual: true }, { state: { phase: 'current' }, manual: true }])
  expect(f.downloadUpdate).not.toHaveBeenCalled()
  const development = fixture(false)
  await expect(development.controller.check(true)).resolves.toEqual({ phase: 'unavailable' })
  expect(development.checkForUpdates).not.toHaveBeenCalled()
})

it('joins a manual request to the startup check and its background download', async () => {
  const f = fixture()
  const started = Promise.withResolvers<undefined>()
  const result = Promise.withResolvers<CheckedUpdate>()
  f.checkForUpdates.mockImplementationOnce(() => { started.resolve(undefined); return result.promise })
  const startup = f.controller.check()
  await started.promise
  const manual = f.controller.check(true)
  expect(manual).toBe(startup)
  expect(f.states.at(-1)).toEqual({ state: { phase: 'checking' }, manual: true })
  result.resolve(checked())
  await Promise.all([startup, manual])
  expect(f.checkForUpdates).toHaveBeenCalledOnce()
  expect(f.downloadUpdate).toHaveBeenCalledOnce()
  expect(f.states).toContainEqual({ state: { phase: 'downloading', version: '1.1.0', percent: 42 }, manual: true })
  expect(f.hooks.confirm).toHaveBeenCalledOnce()
})

it('requires both the prepared event and download settlement before offering installation', async () => {
  const f = fixture()
  const started = Promise.withResolvers<undefined>()
  const prepared = Promise.withResolvers<string[]>()
  f.downloadUpdate.mockImplementationOnce(() => { f.prepared(); started.resolve(undefined); return prepared.promise })
  const pending = f.controller.check()
  await started.promise
  expect(f.controller.state.phase).toBe('downloading')
  expect(f.hooks.confirm).not.toHaveBeenCalled()
  expect(f.controller.check(true)).toBe(pending)
  prepared.resolve(['prepared-installer.exe'])
  await pending
  expect(f.hooks.confirm).toHaveBeenCalledOnce()
  const missing = fixture()
  missing.downloadUpdate.mockImplementationOnce(async () => { missing.prepared('2.0.0'); return ['unmatched.exe'] })
  await expect(missing.controller.check()).resolves.toMatchObject({ phase: 'error', operation: 'download' })
  expect(missing.hooks.confirm).not.toHaveBeenCalled()
})

it.each(['check', 'download'] as const)('contains %s failures and permits an explicit retry', async (operation) => {
  const f = fixture()
  const failure = new Error('Network unavailable')
  if (operation === 'check') f.checkForUpdates.mockRejectedValueOnce(failure)
  else f.downloadUpdate.mockRejectedValueOnce(failure)
  await expect(f.controller.check()).resolves.toMatchObject({ phase: 'error', operation, message: failure.message })
  expect(f.hooks.confirm).not.toHaveBeenCalled()
  expect(f.states.at(-1)?.manual).toBe(false)
  await expect(f.controller.check(true)).resolves.toEqual({ phase: 'ready', version: '1.1.0' })
  expect(f.hooks.confirm).toHaveBeenCalledOnce()
  expect(f.quitAndInstall).not.toHaveBeenCalled()
})

it('treats a missing update result as a check failure', async () => {
  const f = fixture()
  f.checkForUpdates.mockResolvedValueOnce(null)
  await expect(f.controller.check(true)).resolves.toMatchObject({ phase: 'error', operation: 'check' })
  expect(f.downloadUpdate).not.toHaveBeenCalled()
})

it('waits for confirmed saved shutdown before invoking the installer exactly once', async () => {
  const f = fixture()
  const started = Promise.withResolvers<undefined>()
  const saved = Promise.withResolvers<undefined>()
  f.hooks.confirm.mockResolvedValueOnce(true)
  f.hooks.restartWithInstall.mockImplementationOnce(async (install) => {
    started.resolve(undefined)
    await saved.promise
    install(); install()
    return true
  })
  const pending = f.controller.check()
  await started.promise
  expect(f.controller.state).toEqual({ phase: 'installing', version: '1.1.0' })
  expect(f.quitAndInstall).not.toHaveBeenCalled()
  saved.resolve(undefined)
  await pending
  await f.controller.check(true)
  expect(f.hooks.confirm).toHaveBeenCalledOnce()
  expect(f.quitAndInstall).toHaveBeenCalledExactlyOnceWith(true, true)
})

it('offers a prepared update again manually without another check or download', async () => {
  const f = fixture()
  await f.controller.check()
  await f.controller.check()
  expect(f.hooks.confirm).toHaveBeenCalledOnce()
  f.hooks.confirm.mockResolvedValueOnce(true)
  f.hooks.restartWithInstall.mockResolvedValueOnce(false)
  await expect(f.controller.check(true)).resolves.toEqual({ phase: 'ready', version: '1.1.0' })
  expect(f.checkForUpdates).toHaveBeenCalledOnce()
  expect(f.downloadUpdate).toHaveBeenCalledOnce()
  expect(f.quitAndInstall).not.toHaveBeenCalled()
})

it('keeps failed shutdown recoverable and rejects an install callback retained after cancellation', async () => {
  const f = fixture()
  f.hooks.confirm.mockResolvedValue(true)
  f.hooks.restartWithInstall.mockRejectedValueOnce(new Error('Draft save failed'))
  await expect(f.controller.check()).resolves.toMatchObject({ phase: 'error', operation: 'install', message: 'Draft save failed' })
  expect(f.quitAndInstall).not.toHaveBeenCalled()
  let staleInstall: (() => void) | undefined
  f.hooks.restartWithInstall.mockImplementationOnce(async (install) => { staleInstall = install; return false })
  await expect(f.controller.check(true)).resolves.toEqual({ phase: 'ready', version: '1.1.0' })
  staleInstall?.()
  expect(f.quitAndInstall).not.toHaveBeenCalled()
})

it('retains updater errors emitted after installation handoff', async () => {
  const f = fixture()
  f.hooks.confirm.mockResolvedValueOnce(true)
  await f.controller.check()
  f.events.emit('error', new Error('Installer could not start'))
  expect(f.controller.state).toEqual({ phase: 'error', operation: 'install', version: '1.1.0', message: 'Installer could not start' })
  expect(f.states.at(-1)?.manual).toBe(true)
})

it('suppresses disposed checks and consumes late network errors until their promise settles', async () => {
  const f = fixture()
  const started = Promise.withResolvers<undefined>()
  const result = Promise.withResolvers<CheckedUpdate>()
  f.checkForUpdates.mockImplementationOnce(() => { started.resolve(undefined); return result.promise })
  const pending = f.controller.check()
  await started.promise
  const count = f.states.length
  f.controller.dispose()
  expect(() => f.events.emit('error', new Error('Late network failure'))).not.toThrow()
  result.reject(new Error('Disconnected'))
  await pending
  await Promise.resolve()
  expect(f.events.listenerCount('error')).toBe(0)
  expect(f.events.listenerCount('download-progress')).toBe(0)
  expect(f.events.listenerCount('update-downloaded')).toBe(0)
  expect(f.states).toHaveLength(count)
  expect(f.downloadUpdate).not.toHaveBeenCalled()
  await f.controller.check(true)
  expect(f.checkForUpdates).toHaveBeenCalledOnce()
})

it('does not install after disposal while a confirmation or saved shutdown is pending', async () => {
  const f = fixture()
  const prompted = Promise.withResolvers<undefined>()
  const answer = Promise.withResolvers<boolean>()
  f.hooks.confirm.mockImplementationOnce(() => { prompted.resolve(undefined); return answer.promise })
  const pending = f.controller.check()
  await prompted.promise
  f.controller.dispose()
  answer.resolve(true)
  await pending
  expect(f.hooks.restartWithInstall).not.toHaveBeenCalled()
  expect(f.quitAndInstall).not.toHaveBeenCalled()

  const closing = fixture()
  const started = Promise.withResolvers<undefined>()
  const saved = Promise.withResolvers<undefined>()
  closing.hooks.confirm.mockResolvedValueOnce(true)
  closing.hooks.restartWithInstall.mockImplementationOnce(async (install) => {
    started.resolve(undefined); await saved.promise; install(); return true
  })
  const installing = closing.controller.check()
  await started.promise
  closing.controller.dispose()
  saved.resolve(undefined)
  await installing
  expect(closing.quitAndInstall).not.toHaveBeenCalled()
})

it.each(['throw', 'reject'] as const)('contains a native notice %s without losing the prepared update', async (mode) => {
  const f = fixture()
  const log = vi.spyOn(console, 'error').mockImplementation(() => {})
  f.hooks.notice.mockImplementationOnce(() => {
    const error = new Error('Window closed')
    if (mode === 'throw') throw error
    return Promise.reject(error)
  })
  await expect(f.controller.check()).resolves.toEqual({ phase: 'ready', version: '1.1.0' })
  expect(log).toHaveBeenCalledWith('RainyAgent update notice failed:', expect.any(Error))
})
