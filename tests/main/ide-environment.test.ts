/** Human admission, exact WSL command targets, and quiescent development setup. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createIdeEnvironmentSetup, parseIdeEnvironmentAction, resolveIdeEnvironmentConfig } from '../src/ide-environment.ts'
import type { IdeEnvironmentInspection, IdeEnvironmentPlatform, IdeEnvironmentSetup } from '../src/ide-environment.ts'
import { createWindowsIdeEnvironmentPlatform } from '../src/ide-environment-platform.ts'
import type { IdeEnvironmentCommand } from '../src/ide-environment-platform.ts'

const owners: IdeEnvironmentSetup[] = []
const releases: Array<() => void> = []
afterEach(async () => {
  for (const release of releases.splice(0)) release()
  await Promise.all(owners.splice(0).map(owner => owner.close()))
  vi.restoreAllMocks()
})

function observation(): IdeEnvironmentInspection {
  return { os: 'ubuntu', osVersion: '26.04', architecture: 'amd64', mediaPresent: true, mediaMatches: true, packageCount: 92,
    incompletePackages: ['gdb'], tools: [{ name: 'python3', path: '/usr/bin/python3', ready: true }, { name: 'gdb', path: null, ready: false }] }
}

function fixture() {
  let observed = observation()
  const ready = (): void => {
    observed = { ...observed, incompletePackages: [],
      tools: observed.tools.map(tool => ({ ...tool, path: `/usr/bin/${tool.name}`, ready: true })) }
  }
  const platform: IdeEnvironmentPlatform = { inspect: vi.fn(async () => observed), install: vi.fn(async () => { ready() }) }
  const progress = vi.fn()
  const config = resolveIdeEnvironmentConfig()
  const owner = createIdeEnvironmentSetup({ distro: 'Selected Distro', platform, config, onProgress: progress })
  owners.push(owner)
  return { owner, platform, progress, config, ready,
    change: (fields: Partial<IdeEnvironmentInspection>) => { observed = { ...observed, ...fields } } }
}

describe('development setup admission', () => {
  it('does not probe at construction and never installs during inspection or retry', async () => {
    const { owner, platform } = fixture()
    expect(platform.inspect).not.toHaveBeenCalled()
    expect(platform.install).not.toHaveBeenCalled()
    expect(await owner.inspect()).toMatchObject({ status: 'needs-install', distro: 'Selected Distro', busy: false })
    expect(await owner.act({ type: 'retry' })).toMatchObject({ status: 'needs-install' })
    expect(platform.install).not.toHaveBeenCalled()
    expect(() => parseIdeEnvironmentAction({ type: 'install', distro: 'Another Distro' })).toThrow()
    expect(() => parseIdeEnvironmentAction({ type: 'run', command: 'arbitrary' })).toThrow()
  })

  it.each([
    { os: 'debian', osVersion: '13', architecture: 'amd64' },
    { os: 'ubuntu', osVersion: '24.04', architecture: 'amd64' },
    { os: 'ubuntu', osVersion: '26.04', architecture: 'aarch64' },
  ])('refuses Ubuntu media for an unsupported distribution: %j', async (unsupported) => {
    const { owner, platform, change } = fixture()
    change(unsupported)
    expect(await owner.act({ type: 'install' })).toMatchObject({ status: 'unsupported', offlineInstallSupported: false })
    expect(platform.install).not.toHaveBeenCalled()
  })

  it('refuses missing media and does not install already-ready tools', async () => {
    const { owner, platform, change, ready } = fixture()
    change({ mediaPresent: false })
    expect(await owner.act({ type: 'install' })).toMatchObject({ status: 'missing-media' })
    expect(platform.install).not.toHaveBeenCalled()
    ready()
    expect(await owner.act({ type: 'install' })).toMatchObject({ status: 'ready' })
    expect(platform.install).not.toHaveBeenCalled()
  })

  it('coalesces explicit installs, publishes progress, and verifies readiness after the installer exits', async () => {
    const { owner, platform, ready } = fixture()
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    releases.push(() => { release.resolve(undefined) })
    vi.mocked(platform.install).mockImplementation(async (progress) => {
      progress('Installing packages')
      entered.resolve(undefined)
      await release.promise
      ready()
    })
    const first = owner.act({ type: 'install' })
    await entered.promise
    const second = owner.act({ type: 'install' })
    expect(await owner.inspect()).toMatchObject({ status: 'installing', busy: true, log: ['Installing packages'] })
    expect(platform.install).toHaveBeenCalledOnce()
    release.resolve(undefined)
    const results = await Promise.all([first, second])
    expect(results[0]).toEqual(results[1])
    expect(results[0]).toMatchObject({ status: 'ready', busy: false })
    expect(platform.inspect).toHaveBeenCalledTimes(2)
  })

  it('requires another install click after failure; retry only checks the environment', async () => {
    const { owner, platform } = fixture()
    vi.mocked(platform.install).mockRejectedValueOnce(new Error('offline media damaged'))
    expect(await owner.act({ type: 'install' })).toMatchObject({ status: 'error', message: 'offline media damaged' })
    expect(await owner.act({ type: 'retry' })).toMatchObject({ status: 'needs-install' })
    expect(platform.install).toHaveBeenCalledTimes(1)
    expect(await owner.act({ type: 'install' })).toMatchObject({ status: 'ready' })
    expect(platform.install).toHaveBeenCalledTimes(2)
  })

  it('does not report ready when the installer exits without preparing the required executables', async () => {
    const { owner, platform } = fixture()
    vi.mocked(platform.install).mockResolvedValue(undefined)
    expect(await owner.act({ type: 'install' })).toMatchObject({ status: 'error', code: 'development-tools-incomplete' })
  })

  it('drains an admitted install on close and suppresses late progress callbacks', async () => {
    const { owner, platform, ready, progress } = fixture()
    const entered = Promise.withResolvers<undefined>()
    const release = Promise.withResolvers<undefined>()
    releases.push(() => { release.resolve(undefined) })
    vi.mocked(platform.install).mockImplementation(async (notify) => {
      entered.resolve(undefined)
      await release.promise
      notify('late completion')
      ready()
    })
    const install = owner.act({ type: 'install' })
    await entered.promise
    const callbacks = progress.mock.calls.length
    let finished = false
    const closing = owner.close().then(() => { finished = true })
    expect(finished).toBe(false)
    await expect(owner.act({ type: 'retry' })).rejects.toMatchObject({ code: 'setup-closed' })
    release.resolve(undefined)
    await install
    await closing
    expect(finished).toBe(true)
    expect(progress).toHaveBeenCalledTimes(callbacks)
  })
})

describe('development WSL command targets', () => {
  it('keeps probes unprivileged and reserves root solely for the fixed verified offline installer', async () => {
    const calls: IdeEnvironmentCommand[] = []
    const observed = observation()
    const execute = vi.fn(async (command: IdeEnvironmentCommand) => {
      calls.push(command)
      return command.args.includes('root') ? 'apt progress\n{"installedPackages":92,"offline":true}\n' : JSON.stringify(observed)
    })
    const platform = createWindowsIdeEnvironmentPlatform({ distro: 'Chosen Distro', resourceRoot: '/home/user/runtime/app/resources/ide',
      config: resolveIdeEnvironmentConfig(), execute })
    expect(execute).not.toHaveBeenCalled()
    await platform.inspect()
    expect(calls[0]?.args.slice(0, 5)).toEqual(['-d', 'Chosen Distro', '--exec', 'python3', '-c'])
    expect(calls[0]?.args).not.toContain('root')
    await platform.install(() => {})
    expect(calls[2]?.args).toEqual(['-d', 'Chosen Distro', '-u', 'root', '--exec', 'python3',
      '/home/user/runtime/app/resources/ide/install-system-packages.py', '/home/user/runtime/app/resources/ide/system-packages'])
    expect(calls[2]?.timeoutMs).toBe(0)
  })

  it('rechecks compatibility before direct install and rejects missing completion evidence', async () => {
    let observed = { ...observation(), os: 'debian' }
    const execute = vi.fn(async (command: IdeEnvironmentCommand) => command.args.includes('root') ? 'exited without confirmation' : JSON.stringify(observed))
    const platform = createWindowsIdeEnvironmentPlatform({ distro: 'fixed', resourceRoot: '/resources/ide', config: resolveIdeEnvironmentConfig(), execute })
    await expect(platform.install(() => {})).rejects.toMatchObject({ code: 'unsupported-distribution' })
    expect(execute).toHaveBeenCalledTimes(1)
    observed = observation()
    await expect(platform.install(() => {})).rejects.toMatchObject({ code: 'development-install-unconfirmed' })
    expect(execute).toHaveBeenCalledTimes(3)
  })
})
