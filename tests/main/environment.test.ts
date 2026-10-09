/** Recovery and concurrency checks for desktop environment preparation. */
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { spawn } from 'node:child_process'
import { createRequire } from 'node:module'
import { pathToFileURL } from 'node:url'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createEnvironmentSetup, environmentStateDirectory, EnvironmentSetupError } from '../../src/main/environment.ts'
import type { EnvironmentPlatform, EnvironmentSetupOptions, EnvironmentSystem } from '../../src/main/environment.ts'
import { createWindowsEnvironmentPlatform } from '../../src/main/environment-platform.ts'
import { quotePowerShell } from '../../src/main/powershell.ts'
import { execFileSync } from 'node:child_process'

const temporaryRoots: string[] = []
afterEach(async () => {
  await Promise.all(temporaryRoots.splice(0).map(path => rm(path, { recursive: true, force: true })))
})

async function fixture(initial: Record<string, unknown> = {}) {
  const root = await mkdtemp(join(tmpdir(), 'rainy-environment-'))
  temporaryRoots.push(root)
  let settings = initial
  const system: EnvironmentSystem = {
    supported: true, virtualization: true, wslInstalled: true, componentsEnabled: true, bootId: 'boot-one',
    distributions: [{ name: 'PersonalUbuntu', version: 2, basePath: 'personal' }],
  }
  const platform: EnvironmentPlatform = {
    inspectSystem: vi.fn(async () => system),
    checkDistribution: vi.fn(async () => {}),
    acquireLock: vi.fn(async () => async () => {}),
    installSystem: vi.fn(async () => { system.wslInstalled = true; system.componentsEnabled = true; return { rebootRequired: false } }),
    createManagedDistribution: vi.fn(async (name: string) => { system.distributions.push({ name, version: 2, basePath: 'managed' }) }),
  }
  const options: EnvironmentSetupOptions = {
    installRoot: join(root, 'install'), userData: join(root, 'profile'), platform,
    readDesktopSettings: async () => settings,
    writeDesktopSettings: async (value) => { settings = value },
  }
  return { root, system, platform, options, settings: () => settings, controller: createEnvironmentSetup(options) }
}

describe('environment setup', () => {
  it('keeps a healthy saved distribution without taking an install lock or changing Windows', async () => {
    const test = await fixture({ distro: 'PersonalUbuntu', theme: 'dark' })
    expect(await test.controller.inspect()).toMatchObject({ status: 'ready', distro: 'PersonalUbuntu' })
    expect(test.platform.checkDistribution).toHaveBeenCalledWith('PersonalUbuntu')
    expect(test.platform.acquireLock).not.toHaveBeenCalled()
    expect(test.platform.installSystem).not.toHaveBeenCalled()
    expect(test.platform.createManagedDistribution).not.toHaveBeenCalled()
  })

  it('offers a clean environment instead of silently adopting a personal distribution', async () => {
    const test = await fixture({ theme: 'light' })
    expect(await test.controller.inspect()).toMatchObject({ status: 'needs-distro' })
    expect(test.settings()).toEqual({ theme: 'light' })
    expect(test.platform.checkDistribution).not.toHaveBeenCalled()
  })

  it('preserves a missing saved distribution until the user explicitly chooses an alternative', async () => {
    const test = await fixture({ distro: 'RemovedUbuntu', theme: 'dark' })
    expect(await test.controller.inspect()).toMatchObject({ status: 'saved-distro-missing', savedDistro: 'RemovedUbuntu' })
    expect(test.settings().distro).toBe('RemovedUbuntu')
    expect(await test.controller.act({ type: 'select-existing', distroName: 'PersonalUbuntu' })).toMatchObject({ status: 'ready', distro: 'PersonalUbuntu' })
    expect(test.settings()).toEqual({ distro: 'PersonalUbuntu', theme: 'dark' })
    expect(test.platform.installSystem).not.toHaveBeenCalled()
  })

  it('does not elevate during inspection, retry, or an attempted resume before system setup', async () => {
    const test = await fixture()
    test.system.wslInstalled = false
    test.system.componentsEnabled = false
    expect(await test.controller.inspect()).toMatchObject({ status: 'needs-system' })
    await test.controller.act({ type: 'retry' })
    await test.controller.act({ type: 'resume' })
    expect(test.platform.installSystem).not.toHaveBeenCalled()
    expect(test.platform.createManagedDistribution).not.toHaveBeenCalled()
    expect(await test.controller.act({ type: 'install-system-components' })).toMatchObject({ status: 'needs-distro' })
    expect(test.platform.installSystem).toHaveBeenCalledTimes(1)
  })

  it('requires a reboot after a process interruption leaves installed system components without a completion record', async () => {
    const test = await fixture()
    test.system.wslInstalled = false
    test.platform.installSystem = vi.fn(async () => { test.system.wslInstalled = true; throw new EnvironmentSetupError('interrupted', 'Installer interrupted') })
    expect(await test.controller.act({ type: 'install-system-components' })).toMatchObject({ status: 'error', code: 'interrupted' })
    expect(await createEnvironmentSetup(test.options).inspect()).toMatchObject({ status: 'reboot-required' })
    test.system.bootId = 'boot-two'
    expect(await createEnvironmentSetup(test.options).inspect()).toMatchObject({ status: 'needs-distro' })
  })

  it('retains the reboot requirement across controller restarts and resumes on a new Windows boot', async () => {
    const test = await fixture()
    test.system.wslInstalled = false
    test.platform.installSystem = vi.fn(async () => { test.system.wslInstalled = true; return { rebootRequired: true } })
    expect(await test.controller.act({ type: 'install-system-components' })).toMatchObject({ status: 'reboot-required' })
    const restarted = createEnvironmentSetup(test.options)
    expect(await restarted.inspect()).toMatchObject({ status: 'reboot-required' })
    await restarted.act({ type: 'resume' })
    expect(test.platform.createManagedDistribution).not.toHaveBeenCalled()
    test.system.bootId = 'boot-two'
    expect(await restarted.inspect()).toMatchObject({ status: 'needs-distro' })
    expect(await restarted.act({ type: 'resume' })).toMatchObject({ status: 'ready' })
    expect(test.platform.installSystem).toHaveBeenCalledTimes(1)
  })

  it('coalesces overlapping create requests while exposing progress without launching another import', async () => {
    const test = await fixture()
    const entered = Promise.withResolvers<undefined>()
    const finish = Promise.withResolvers<undefined>()
    test.platform.createManagedDistribution = vi.fn(async (name: string, progress: (message: string) => void) => {
      progress('Import in progress')
      entered.resolve(undefined)
      await finish.promise
      test.system.distributions.push({ name, version: 2, basePath: 'managed' })
    })
    const first = test.controller.act({ type: 'create-managed-distro' })
    await entered.promise
    const second = test.controller.act({ type: 'create-managed-distro' })
    expect(await test.controller.inspect()).toMatchObject({ status: 'working', busy: true, message: 'Import in progress' })
    expect(test.platform.createManagedDistribution).toHaveBeenCalledTimes(1)
    finish.resolve(undefined)
    const results = await Promise.all([first, second])
    expect(results[0]).toEqual(results[1])
    expect(results[0].status).toBe('ready')
    expect(test.platform.acquireLock).toHaveBeenCalledTimes(1)
  })

  it('recovers the same owned distribution after an interrupted create without adopting personal data', async () => {
    const test = await fixture()
    const names: string[] = []
    test.platform.createManagedDistribution = vi.fn(async (name: string) => {
      names.push(name)
      if (names.length === 1) throw new EnvironmentSetupError('interrupted', 'Interrupted import')
      test.system.distributions.push({ name, version: 2, basePath: 'managed' })
    })
    expect(await test.controller.act({ type: 'create-managed-distro' })).toMatchObject({ status: 'error', code: 'interrupted' })
    const record: unknown = JSON.parse(await readFile(join(environmentStateDirectory(test.options.installRoot, test.options.userData), 'setup.json'), 'utf8'))
    expect(record).toMatchObject({ phase: 'creating-distro' })
    expect(await createEnvironmentSetup(test.options).act({ type: 'resume' })).toMatchObject({ status: 'ready' })
    expect(names[0]).toBe(names[1])
    expect(test.settings().distro).toBe(names[0])
  })

  it('protects a corrupt journal and refuses subsequent create or install actions', async () => {
    const test = await fixture()
    const directory = environmentStateDirectory(test.options.installRoot, test.options.userData)
    await mkdir(directory, { recursive: true })
    await writeFile(join(directory, 'setup.json'), '{broken')
    expect(await test.controller.inspect()).toMatchObject({ status: 'error', code: 'journal-invalid' })
    await test.controller.act({ type: 'create-managed-distro' })
    await test.controller.act({ type: 'install-system-components' })
    expect(test.platform.createManagedDistribution).not.toHaveBeenCalled()
    expect(test.platform.installSystem).not.toHaveBeenCalled()
    expect(await readFile(join(directory, 'setup.json'), 'utf8')).toBe('{broken')
  })

  it('does not overwrite settings when the selected environment fails health checks', async () => {
    const test = await fixture({ distro: 'Missing', theme: 'dark' })
    test.platform.checkDistribution = vi.fn(async () => { throw new EnvironmentSetupError('distro-unhealthy', 'Python is missing') })
    expect(await test.controller.act({ type: 'select-existing', distroName: 'PersonalUbuntu' })).toMatchObject({ status: 'error', code: 'distro-unhealthy' })
    expect(test.settings()).toEqual({ distro: 'Missing', theme: 'dark' })
  })

  it('reports a cross-process setup lock without creating or updating setup records', async () => {
    const test = await fixture()
    test.platform.acquireLock = vi.fn(async () => { throw new EnvironmentSetupError('setup-busy', 'Already running') })
    expect(await test.controller.act({ type: 'create-managed-distro' })).toMatchObject({ status: 'error', code: 'setup-busy' })
    expect(test.platform.createManagedDistribution).not.toHaveBeenCalled()
    expect(test.settings()).toEqual({})
  })

  it('blocks unsupported hardware before any installation action', async () => {
    const test = await fixture()
    test.system.virtualization = false
    expect(await test.controller.act({ type: 'create-managed-distro' })).toMatchObject({ status: 'blocked', code: 'virtualization-disabled' })
    expect(test.platform.createManagedDistribution).not.toHaveBeenCalled()
    expect(test.platform.installSystem).not.toHaveBeenCalled()
  })

  it('allocates a real OS lock exclusively and releases it before the next owner enters', async () => {
    const test = await fixture()
    const first = createWindowsEnvironmentPlatform(test.options)
    const second = createWindowsEnvironmentPlatform(test.options)
    const release = await first.acquireLock()
    try { await expect(second.acquireLock()).rejects.toMatchObject({ code: 'setup-busy' }) } finally { await release() }
    const releaseNext = await second.acquireLock()
    await releaseNext()
  })

  it('refuses missing offline media before requesting elevation or starting WSL import', async () => {
    const test = await fixture()
    const native = createWindowsEnvironmentPlatform(test.options)
    await expect(native.createManagedDistribution('UnusedTestName', () => {})).rejects.toMatchObject({ code: 'media-missing' })
  })

  it('holds the installation lock across independent Node processes and releases it on clean exit', async () => {
    const test = await fixture()
    const ready = Promise.withResolvers<undefined>()
    const stopped = Promise.withResolvers<undefined>()
    const moduleUrl = new URL('../../src/main/environment-platform.ts', import.meta.url).href
    const script = `import { createWindowsEnvironmentPlatform } from ${JSON.stringify(moduleUrl)}; const platform = createWindowsEnvironmentPlatform(${JSON.stringify({ installRoot: test.options.installRoot, userData: test.options.userData })}); const release = await platform.acquireLock(); process.once('message', async () => { await release(); process.disconnect(); }); process.send('locked');`
    const child = spawn(process.execPath, ['--import', pathToFileURL(createRequire(import.meta.url).resolve('tsx/esm')).href, '--input-type=module', '-e', script], { windowsHide: true, stdio: ['ignore', 'ignore', 'pipe', 'ipc'] })
    let stderr = ''
    child.stderr?.on('data', (value: Buffer) => { stderr += value.toString() })
    child.once('message', (message) => { if (message === 'locked') ready.resolve(undefined) })
    child.once('error', (error) => { ready.reject(error); stopped.resolve(undefined) })
    child.once('exit', (code) => { ready.reject(new Error(`Lock owner exited ${code}: ${stderr}`)); stopped.resolve(undefined) })
    try {
      await ready.promise
      await expect(createWindowsEnvironmentPlatform(test.options).acquireLock()).rejects.toMatchObject({ code: 'setup-busy' })
    } finally {
      if (child.connected) child.send('release')
      await stopped.promise
    }
    const release = await createWindowsEnvironmentPlatform(test.options).acquireLock()
    await release()
  })

  it('keeps the selected healthy distribution after moving the application to another installation directory', async () => {
    const test = await fixture({ distro: 'PersonalUbuntu', theme: 'dark' })
    await test.controller.act({ type: 'select-existing', distroName: 'PersonalUbuntu' })
    const originalPath = join(environmentStateDirectory(test.options.installRoot, test.options.userData), 'setup.json')
    const original = await readFile(originalPath, 'utf8')
    const moved = { ...test.options, installRoot: join(test.root, 'other-volume', 'RainyAgent') }
    expect(await createEnvironmentSetup(moved).inspect()).toMatchObject({ status: 'ready', distro: 'PersonalUbuntu' })
    const originalDirectory = environmentStateDirectory(test.options.installRoot, test.options.userData)
    expect(environmentStateDirectory(moved.installRoot, moved.userData)).not.toBe(originalDirectory)
    expect(await readFile(originalPath, 'utf8')).toBe(original)
    expect(test.settings()).toEqual({ distro: 'PersonalUbuntu', theme: 'dark' })
  })

  it('does not let another installation legacy journal block a healthy saved distribution or overwrite the old record', async () => {
    const test = await fixture({ distro: 'PersonalUbuntu' })
    const snapshot = await test.controller.inspect()
    const legacyPath = join(test.options.userData, 'environment', 'setup.json')
    await mkdir(join(test.options.userData, 'environment'), { recursive: true })
    const legacy = JSON.stringify({ version: 1, installRoot: test.options.installRoot, managedDistro: snapshot.managedDistro, phase: 'awaiting-reboot', bootId: 'boot-one' })
    await writeFile(legacyPath, legacy)
    const moved = { ...test.options, installRoot: join(test.root, 'relocated') }
    expect(await createEnvironmentSetup(moved).inspect()).toMatchObject({ status: 'ready', distro: 'PersonalUbuntu' })
    expect(await readFile(legacyPath, 'utf8')).toBe(legacy)
  })

  it('resumes a matching legacy record into the new directory while preserving the global record unchanged', async () => {
    const test = await fixture()
    const snapshot = await test.controller.inspect()
    const legacyPath = join(test.options.userData, 'environment', 'setup.json')
    await mkdir(join(test.options.userData, 'environment'), { recursive: true })
    const legacy = JSON.stringify({ version: 1, installRoot: test.options.installRoot, managedDistro: snapshot.managedDistro, phase: 'awaiting-reboot', bootId: 'boot-one' })
    await writeFile(legacyPath, legacy)
    expect(await createEnvironmentSetup(test.options).inspect()).toMatchObject({ status: 'reboot-required' })
    test.system.bootId = 'boot-two'
    expect(await createEnvironmentSetup(test.options).act({ type: 'resume' })).toMatchObject({ status: 'ready' })
    expect(await readFile(legacyPath, 'utf8')).toBe(legacy)
    const path = join(environmentStateDirectory(test.options.installRoot, test.options.userData), 'setup.json')
    expect(JSON.parse(await readFile(path, 'utf8'))).toMatchObject({ phase: 'ready' })
  })
})

describe('elevated PowerShell quoting', () => {
  it('doubles every quote PowerShell treats as a single-quote delimiter', () => {
    expect(quotePowerShell(String.raw`C:\Users\O’Brien's`)).toBe(String.raw`'C:\Users\O’’Brien''s'`)
  })

  it.runIf(process.platform === 'win32')('round-trips a profile path with a typographic apostrophe', () => {
    const path = String.raw`C:\Users\O’Brien\AppData\report.json`
    const script = `[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); Write-Output ${quotePowerShell(path)}`
    const output = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand',
      Buffer.from(script, 'utf16le').toString('base64')], { encoding: 'utf8', windowsHide: true })
    expect(output.trim()).toBe(path)
  })
})
