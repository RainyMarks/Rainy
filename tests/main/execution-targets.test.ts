/** Desktop preference updates keep every field when the language and the execution target are saved at once. */
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, expect, it, vi } from 'vitest'
import { readDesktopPreferences, removeSshHost, saveDesktopLocale, saveExecutionTarget, saveShellIntegration, saveSshHost, savedExecutionTarget, sshTarget,
  sshTargets, WINDOWS_TARGET } from '../../src/main/execution-targets.ts'
import { ExecutionTargetId } from '../../src/shared/project-registry.ts'

const roots: string[] = []
afterEach(async () => { await Promise.all(roots.splice(0).map(path => rm(path, { recursive: true, force: true }))) })

async function preferencesFile(initial: Record<string, unknown>): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), 'rainy-desktop-'))
  roots.push(root)
  const path = join(root, 'desktop.json')
  await writeFile(path, JSON.stringify(initial))
  return path
}

it('applies concurrent language and target updates in order without losing either', async () => {
  const path = await preferencesFile({ locale: 'en', unrelated: 1 })
  const wsl = { id: ExecutionTargetId('wsl:ubuntu'), kind: 'wsl' as const, label: 'WSL · Ubuntu', distro: 'Ubuntu' }
  await Promise.all([saveDesktopLocale(path, 'zh'), saveExecutionTarget(path, wsl), saveDesktopLocale(path, 'en')])
  expect(await readDesktopPreferences(path)).toEqual({
    locale: 'en', unrelated: 1, executionTarget: { id: 'wsl:ubuntu', kind: 'wsl', label: 'WSL · Ubuntu', distro: 'Ubuntu' }, distro: 'Ubuntu',
    pendingProject: null,
  })
})

it('saves each Explorer switch and the remembered keys apart, dropping 3.9.0\'s on-by-default choice', async () => {
  const path = await preferencesFile({ locale: 'zh', shellIntegration: { contextMenu: true, fileTypes: true } })
  await saveShellIntegration(path, { contextMenu: true })
  expect(await readDesktopPreferences(path)).toEqual({ locale: 'zh', explorerIntegration: { contextMenu: true, fileTypes: false } })
  // A background sync saving its keys keeps a switch flipped meanwhile, and a switch keeps the keys.
  await Promise.all([saveShellIntegration(path, { fileTypes: true }), saveShellIntegration(path, { keep: ['software\\classes\\.txt'] })])
  await saveShellIntegration(path, { contextMenu: false })
  expect((await readDesktopPreferences(path)).explorerIntegration).toEqual({ contextMenu: false, fileTypes: true, keep: ['software\\classes\\.txt'] })
  await saveShellIntegration(path, { fileTypes: false, keep: [] })
  expect((await readDesktopPreferences(path)).explorerIntegration).toEqual({ contextMenu: false, fileTypes: false })
})

it('keeps saving after an update fails', async () => {
  const path = await preferencesFile({})
  await writeFile(path, '{ not json')
  await expect(saveDesktopLocale(path, 'zh')).rejects.toThrow()
  await writeFile(path, '{}')
  await saveExecutionTarget(path, WINDOWS_TARGET)
  expect(JSON.parse(await readFile(path, 'utf8'))).toEqual({ executionTarget: { id: 'windows-local', kind: 'windows', label: 'Windows' }, pendingProject: null })
})

it('lists SSH hosts added in Rainy before the aliases of ~/.ssh/config, and forgets only the added ones', async () => {
  const path = await preferencesFile({ sshHosts: [{ destination: '-bad' }, { destination: 'dev' }] })
  const config = join(path, '..', 'config')
  await writeFile(config, 'Host dev lab\n  HostName 10.0.0.2\nHost *.corp\n')
  vi.stubEnv('RAINY_SSH_CONFIG', config)
  try {
    await saveSshHost(path, { destination: 'alice@10.0.0.5', port: 2222 })
    await saveSshHost(path, { destination: 'alice@10.0.0.5', port: 2222 })
    const targets = await sshTargets(await readDesktopPreferences(path))
    expect(targets.map(target => [target.id, target.label, target.saved])).toEqual([
      ['ssh:dev', 'SSH · dev', true], ['ssh:alice@10.0.0.5:2222', 'SSH · alice@10.0.0.5:2222', true], ['ssh:lab', 'SSH · lab', false]])
    await removeSshHost(path, 'ssh:alice@10.0.0.5:2222')
    expect((await readDesktopPreferences(path)).sshHosts).toEqual([{ destination: 'dev' }])
  } finally { vi.unstubAllEnvs() }
})

it('records an SSH target and reads it back for the next launch', async () => {
  const path = await preferencesFile({})
  const target = sshTarget({ destination: 'alice@10.0.0.5', port: 2222 })
  await saveExecutionTarget(path, target)
  expect(savedExecutionTarget(await readDesktopPreferences(path))).toEqual({
    id: 'ssh:alice@10.0.0.5:2222', kind: 'ssh', label: 'SSH · alice@10.0.0.5:2222', destination: 'alice@10.0.0.5', port: 2222 })
})
