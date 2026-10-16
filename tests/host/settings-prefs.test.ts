/** Interface preferences: appearance choices are validated and stored, and a value a newer version wrote does not break loading. */
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, expect, it } from 'vitest'
import { DEFAULT_PREFS, Settings } from '../../src/host/settings.ts'
import { fontListPrefix, normalizeFont } from '../../src/shared/appearance.ts'

const homes: string[] = []
afterEach(async () => { for (const home of homes.splice(0)) await rm(home, { recursive: true, force: true }) })

async function settings(file?: unknown): Promise<Settings> {
  const home = await mkdtemp(join(tmpdir(), 'rainy-prefs-'))
  homes.push(home)
  if (file !== undefined) await writeFile(join(home, 'settings.json'), JSON.stringify(file))
  const loaded = new Settings(home, {})
  await loaded.load()
  return loaded
}

it('starts with the graphite scheme, the automatic accent and the default fonts', async () => {
  expect((await settings()).prefs()).toMatchObject({ colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true })
  expect(DEFAULT_PREFS).toMatchObject({ colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true })
})

it('stores a color scheme, an accent and font lists, and rejects values outside the choices', async () => {
  const store = await settings()
  await store.setPrefs({ colorScheme: 'midnight', accent: 'teal', uiFont: 'Inter, LXGW WenKai', codeFont: 'JetBrains Mono' })
  expect(store.prefs()).toMatchObject({ colorScheme: 'midnight', accent: 'teal', uiFont: 'Inter, LXGW WenKai', codeFont: 'JetBrains Mono' })
  const bad: unknown[] = [{ colorScheme: 'ocean' }, { accent: 'gold' }, { uiFont: 'Fira Code"; color: red' }, { codeFont: 'a, b, c, d, e' },
    { uiFont: 'x'.repeat(161) }]
  for (const change of bad) await expect(store.setPrefs(change as never)).rejects.toThrow()
  expect(store.prefs()).toMatchObject({ colorScheme: 'midnight', accent: 'teal', uiFont: 'Inter, LXGW WenKai' })
})

it('drops a stored value it does not accept and keeps the rest of the file', async () => {
  const file = { version: 1, models: [], selected: null, globalPrompt: '', mcpServers: [],
    ui: { locale: 'en', theme: 'dark', colorScheme: 'aurora', accent: 'violet', uiFont: 'bad;font', future: true } }
  const store = await settings(file)
  expect(store.prefs()).toMatchObject({ locale: 'en', theme: 'dark', colorScheme: 'graphite', accent: 'violet', uiFont: '' })
  await store.setPrefs({ showUsage: false })
  const written = JSON.parse(await readFile(join(homes[0]!, 'settings.json'), 'utf8')) as { ui: Record<string, unknown> }
  expect(written.ui).toEqual({ locale: 'en', theme: 'dark', accent: 'violet', showUsage: false })
})

it('keeps the agent changes automatically by default, also with a file written before the choice existed', async () => {
  const store = await settings({ version: 1, models: [], selected: null, globalPrompt: '', mcpServers: [], ui: { locale: 'zh' } })
  expect(store.prefs().autoKeepChanges).toBe(true)
  await store.setPrefs({ autoKeepChanges: false })
  expect(store.prefs().autoKeepChanges).toBe(false)
  await expect(store.setPrefs({ autoKeepChanges: 'yes' } as never)).rejects.toThrow()
})

it('normalizes typed font lists and quotes family names for CSS', () => {
  expect(normalizeFont(` 'Fira Code' ,  "霞鹜文楷"`)).toBe('Fira Code, 霞鹜文楷')
  expect(normalizeFont('')).toBe('')
  expect(normalizeFont('Consolas, ')).toBeUndefined()
  expect(normalizeFont('a;b')).toBeUndefined()
  expect(fontListPrefix('Fira Code, monospace')).toBe('"Fira Code", monospace,')
  expect(fontListPrefix('')).toBe('')
  expect(fontListPrefix('bad"name')).toBe('')
})
