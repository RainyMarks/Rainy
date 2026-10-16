// @vitest-environment happy-dom
/** The desktop learns the user's theme and language from the page only after the Host's preferences arrived. */
import { expect, it, vi } from 'vitest'
import type { UiPreferences } from '../../src/shared/rpc.ts'
import { pageAppearance } from '../../src/preload/preload-chrome.ts'
import { deferred, handle } from './settings-harness.tsx'

vi.mock('electron', () => ({ ipcRenderer: { send: vi.fn() } }))
vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)

const saved: UiPreferences = { locale: 'en', theme: 'light', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true }

it('reports neither the default language nor a theme before the Host answered, then the stored choices', async () => {
  const answer = deferred<UiPreferences>()
  handle('prefs.get', () => answer.promise)
  handle('prefs.set', change => ({ ...saved, ...change }))
  const root = document.documentElement
  const { setPrefs } = await import('../../src/renderer/prefs.ts')
  expect(root.lang).toBe('zh-CN')
  expect(root.hasAttribute('data-ds-theme-source')).toBe(false)
  expect(pageAppearance(root)).toEqual({ theme: undefined, locale: undefined })
  answer.resolve(saved)
  await vi.waitFor(() => { expect(root.getAttribute('data-ds-theme-source')).toBe('light') })
  expect(pageAppearance(root)).toEqual({ theme: 'light', locale: 'en' })
  await setPrefs({ theme: 'dark', locale: 'zh' })
  expect(pageAppearance(root)).toEqual({ theme: 'dark', locale: 'zh' })
  expect(document.body.hasAttribute('data-ds-dark-theme')).toBe(true)
})

it('ignores an unknown theme source', () => {
  const root = document.createElement('html')
  root.lang = 'en'
  root.setAttribute('data-ds-theme-source', 'sepia')
  expect(pageAppearance(root)).toEqual({ theme: undefined, locale: undefined })
  root.setAttribute('data-ds-theme-source', 'system')
  expect(pageAppearance(root)).toEqual({ theme: 'system', locale: 'en' })
})
