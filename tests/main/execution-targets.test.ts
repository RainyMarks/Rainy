/** Desktop preference updates keep every field when the language and the execution target are saved at once. */
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, expect, it } from 'vitest'
import { readDesktopPreferences, saveDesktopLocale, saveExecutionTarget, WINDOWS_TARGET } from '../../src/main/execution-targets.ts'
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

it('keeps saving after an update fails', async () => {
  const path = await preferencesFile({})
  await writeFile(path, '{ not json')
  await expect(saveDesktopLocale(path, 'zh')).rejects.toThrow()
  await writeFile(path, '{}')
  await saveExecutionTarget(path, WINDOWS_TARGET)
  expect(JSON.parse(await readFile(path, 'utf8'))).toEqual({ executionTarget: { id: 'windows-local', kind: 'windows', label: 'Windows' }, pendingProject: null })
})
