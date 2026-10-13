// @vitest-environment happy-dom
/** The settings window opens on the requested section, closes on Escape, and stores General and Appearance preferences through the Host. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { UiPreferences } from '../../src/shared/rpc.ts'
import { SettingsDialog } from '../../src/renderer/settings/SettingsDialog.tsx'
import type { SettingsDialogProps } from '../../src/renderer/settings/SettingsDialog.tsx'
import { settingsMessages } from '../../src/renderer/settings/messages.ts'
import { getPrefs } from '../../src/renderer/prefs.ts'
import { act } from 'react'
import { all, button, choose, cleanup, click, control, emit, fakeHost, findButton, handle, hasText, render, toast, type, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)
// Canvas measurement is unavailable here; these families count as installed.
vi.mock('../../src/renderer/settings/fonts.ts', async (original) => {
  const actual = await original<typeof import('../../src/renderer/settings/fonts.ts')>()
  const installed = new Set(['Inter', 'KaiTi', 'JetBrains Mono', 'Consolas'])
  return { ...actual, fontInstalled: (family: string) => installed.has(family),
    installedFonts: (candidates: readonly string[]) => candidates.filter(family => installed.has(family)) }
})

const zh = settingsMessages.zh
const en = settingsMessages.en
const defaults: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '' }
let stored: UiPreferences

beforeEach(async () => {
  stored = { ...defaults }
  handle('prefs.set', (change) => { stored = { ...stored, ...change }; return stored })
  await emit('prefs.changed', stored)
})
afterEach(async () => { await cleanup(); delete window.__RAINY_AGENT__ })

function dialog(props: Partial<SettingsDialogProps> = {}) {
  return <SettingsDialog open workspace={null} onClose={() => undefined} {...props} />
}

const panel = (): HTMLElement => {
  const element = document.querySelector<HTMLElement>('[data-settings-dialog]')
  if (element === null) throw new Error('Settings dialog is not open')
  return element
}

it('opens on the requested section, follows section requests and navigates between sections', async () => {
  handle('models.status', () => new Promise(() => undefined))
  const view = await render(dialog({ section: 'memory' }))
  expect(panel().dataset['section']).toBe('memory')
  expect(all('[data-settings-nav]').map(element => element.textContent))
    .toEqual([zh.navGeneral, zh.navAppearance, zh.navModels, zh.navExtensions, zh.navRuntime, zh.navMemory, zh.navSecurity])
  expect(button(zh.navMemory).getAttribute('aria-current')).toBe('page')
  expect(hasText(zh.settingsOpenProject)).toBe(true)
  await click(button(zh.navGeneral))
  expect(panel().dataset['section']).toBe('general')
  await view.rerender(dialog({ section: 'models' }))
  expect(panel().dataset['section']).toBe('models')
  await view.rerender(dialog({ open: false, section: 'models' }))
  expect(document.querySelector('[data-settings-dialog]')).toBeNull()
  await view.rerender(dialog({ section: undefined }))
  expect(panel().dataset['section']).toBe('general')
})

it('keeps an unsaved edit when switching sections, and starts fresh after closing', async () => {
  handle('models.status', () => ({ models: [], credentials: [], selected: null, thinkingLevels: {}, presets: [], globalPrompt: { text: '', maxChars: 4000 } }))
  handle('network.status', () => ({ settings: { mode: 'system' as const }, effective: null, system: null }))
  const view = await render(dialog({ section: 'models' }))
  const prompt = (): HTMLTextAreaElement => document.querySelector<HTMLTextAreaElement>('[data-global-prompt] textarea')!
  await waitFor(() => { expect(prompt().disabled).toBe(false) })
  await type(prompt(), '先读题再动手')
  await click(button(zh.navGeneral))
  expect(panel().dataset['section']).toBe('general')
  await click(button(zh.navModels))
  expect(prompt().value).toBe('先读题再动手')
  await view.rerender(dialog({ open: false, section: 'models' }))
  await view.rerender(dialog({ section: 'models' }))
  await waitFor(() => { expect(prompt().disabled).toBe(false) })
  expect(prompt().value).toBe('')
})

it('closes on Escape, the close button and the mask', async () => {
  const onClose = vi.fn()
  await render(dialog({ onClose }))
  await act(async () => { document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true })) })
  expect(onClose).toHaveBeenCalledTimes(1)
  await click(button(zh.close))
  expect(onClose).toHaveBeenCalledTimes(2)
  await click(document.querySelector<HTMLElement>('[aria-hidden="true"]')!)
  expect(onClose).toHaveBeenCalledTimes(3)
})

it('stores language, appearance, chat preferences and usage display through the Host', async () => {
  await render(dialog())
  expect(button(zh.languageZh).getAttribute('aria-pressed')).toBe('true')
  await click(button(zh.languageEn))
  await waitFor(() => { expect(stored.locale).toBe('en') })
  expect(hasText(en.navGeneral)).toBe(true)
  expect(getPrefs().locale).toBe('en')

  await click(button(en.navAppearance))
  await click(button(en.appearanceDark))
  expect(stored.theme).toBe('dark')
  expect(button(en.appearanceDark).getAttribute('aria-pressed')).toBe('true')
  expect(button(en.appearanceSystem).getAttribute('aria-pressed')).toBe('false')
  expect(document.body.hasAttribute('data-ds-dark-theme')).toBe(true)

  await click(button(en.navGeneral))
  await click(button(en.busyEnterSteer))
  expect(stored.busyEnter).toBe('steer')
  expect(button(en.busyEnterSteer).getAttribute('aria-pressed')).toBe('true')
  await click(button(en.stepDetailDetailed))
  expect(stored.stepDetail).toBe('detailed')
  // Choosing the option already selected stores nothing.
  await click(button(en.stepDetailDetailed))
  await click(button(en.usageTitle))
  expect(stored.showUsage).toBe(false)
  expect(button(en.usageTitle).getAttribute('aria-checked')).toBe('false')
  expect(fakeHost.call.mock.calls.filter(([method]) => method === 'prefs.set').map(([, params]) => params)).toEqual([
    { locale: 'en' }, { theme: 'dark' }, { busyEnter: 'steer' }, { stepDetail: 'detailed' }, { showUsage: false },
  ])
})

it('steps font sizes within 12 to 17 px and resets the appearance to the defaults', async () => {
  await emit('prefs.changed', { ...stored, uiFontSize: 16, codeFontSize: 12 })
  stored = { ...stored, uiFontSize: 16, codeFontSize: 12 }
  await render(dialog({ section: 'appearance' }))
  const value = (field: string): string | null | undefined => document.querySelector(`[data-font-size="${field}"]`)?.textContent
  expect(button(zh.codeFontSizeDecrease).disabled).toBe(true)
  await click(button(zh.fontSizeIncrease))
  expect(value('uiFontSize')).toBe('17')
  expect(button(zh.fontSizeIncrease).disabled).toBe(true)
  expect(document.documentElement.style.getPropertyValue('--dsh-content-font-size')).toBe('17px')
  await click(button(zh.codeFontSizeIncrease))
  expect(value('codeFontSize')).toBe('13')
  await click(button(zh.appearanceReset))
  expect(stored).toMatchObject({ uiFontSize: 14, codeFontSize: 13 })
  expect(button(zh.appearanceReset).disabled).toBe(true)
})

it('applies a color scheme and accent to the page, and previews each scheme in its own colors', async () => {
  await render(dialog({ section: 'appearance' }))
  expect(document.body.getAttribute('data-color-scheme')).toBe('graphite')
  expect(document.body.getAttribute('data-accent')).toBe('blue')
  const swatches = all('[data-scheme-option]')
  expect(swatches.map(swatch => swatch.textContent))
    .toEqual([zh.schemeGraphite, zh.schemeSlate, zh.schemeMidnight, zh.schemeSand, zh.schemeForest, zh.schemeRose, zh.schemeBlack])
  expect(swatches.map(swatch => swatch.querySelector('[data-color-scheme]')?.getAttribute('data-accent')))
    .toEqual(['blue', 'blue', 'violet', 'orange', 'green', 'pink', 'blue'])

  await click(button(zh.schemeMidnight))
  expect(stored.colorScheme).toBe('midnight')
  expect(button(zh.schemeMidnight).getAttribute('aria-pressed')).toBe('true')
  // The automatic accent follows the scheme.
  expect(document.body.getAttribute('data-color-scheme')).toBe('midnight')
  expect(document.body.getAttribute('data-accent')).toBe('violet')
  expect(button(zh.accentAuto).getAttribute('data-accent')).toBe('violet')

  await click(button(zh.accentTeal))
  expect(stored.accent).toBe('teal')
  expect(document.body.getAttribute('data-accent')).toBe('teal')
  expect(all('[data-scheme-option] [data-color-scheme]').every(preview => preview.getAttribute('data-accent') === 'teal')).toBe(true)
  expect(button(zh.accentTeal).getAttribute('aria-pressed')).toBe('true')
  expect(button(zh.accentAuto).getAttribute('aria-pressed')).toBe('false')
  expect(fakeHost.call.mock.calls.filter(([method]) => method === 'prefs.set').map(([, params]) => params))
    .toEqual([{ colorScheme: 'midnight' }, { accent: 'teal' }])
})

it('offers installed fonts, takes typed font names, and puts the chosen fonts first in the page font stacks', async () => {
  await render(dialog({ section: 'appearance' }))
  const root = document.documentElement.style
  await click(button(zh.uiFontTitle))
  expect(all('[role="menuitem"]').map(item => item.textContent)).toEqual([zh.fontDefault, 'Inter', '楷体', zh.fontCustom])
  await click(all<HTMLButtonElement>('[role="menuitem"]')[1]!)
  expect(stored.uiFont).toBe('Inter')
  expect(button(zh.uiFontTitle).textContent).toBe('Inter')
  expect(root.getPropertyValue('--rainy-ui-font')).toBe('"Inter",')

  await choose(zh.codeFontTitle, zh.fontCustom)
  const entry = control(zh.codeFontCustomLabel)
  await type(entry, 'Fira Code; color: red')
  await click(button(zh.fontCustomApply))
  expect(hasText(zh.fontInvalid)).toBe(true)
  expect(stored.codeFont).toBe('')
  await type(entry, ` 'Fira Code' , JetBrains Mono`)
  await act(async () => { entry.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })) })
  await waitFor(() => { expect(stored.codeFont).toBe('Fira Code, JetBrains Mono') })
  expect(entry.value).toBe('Fira Code, JetBrains Mono')
  expect(root.getPropertyValue('--rainy-code-font')).toBe('"Fira Code", "JetBrains Mono",')
  expect(button(zh.codeFontTitle).textContent).toBe(zh.fontCustom)

  // Names this computer lacks fall back, and the row says so.
  await type(entry, 'Nonexistent Sans')
  await click(button(zh.fontCustomApply))
  await waitFor(() => { expect(hasText(zh.fontMissing.replace('{name}', 'Nonexistent Sans'))).toBe(true) })

  await choose(zh.codeFontTitle, zh.fontDefault)
  expect(stored.codeFont).toBe('')
  expect(root.getPropertyValue('--rainy-code-font')).toBe('')
  expect(document.querySelector('[data-custom-font]')).toBeNull()
  await click(button(zh.appearanceReset))
  expect(stored).toMatchObject({ uiFont: '', codeFont: '', colorScheme: 'graphite', accent: 'auto', theme: 'system' })
  expect(root.getPropertyValue('--rainy-ui-font')).toBe('')
})

it('shows the version, links to feedback and lists the keyboard shortcuts from the foot of the navigation', async () => {
  window.__RAINY_AGENT__ = { name: 'Rainy', version: '2.0.0', environment: 'Windows' }
  await render(dialog())
  expect(hasText('当前版本：2.0.0')).toBe(true)
  expect(hasText('版本 2.0.0')).toBe(true)
  expect(findButton(zh.diagnosticsExport)).toBeUndefined()
  const feedback = document.querySelector<HTMLAnchorElement>('[data-settings-footer="feedback"]')!
  expect([feedback.textContent, feedback.getAttribute('href'), feedback.target])
    .toEqual([zh.navFeedback, 'https://github.com/RainyMarks/RainyAgent/issues', '_blank'])
  expect(document.querySelector('[data-shortcuts]')).toBeNull()
  await click(button(zh.shortcutsTitle))
  expect(panel().dataset['section']).toBe('shortcuts')
  expect(button(zh.shortcutsTitle).getAttribute('aria-current')).toBe('page')
  const rows = all('[data-shortcuts] dt').map(row => `${row.textContent}: ${[...row.nextElementSibling!.querySelectorAll('kbd')].map(key => key.textContent).join('+')}`)
  expect(rows).toEqual([
    `${zh.shortcutQuickOpen}: Ctrl+Alt+P`, `${zh.shortcutSettings}: Ctrl+,`, `${zh.shortcutNewChat}: Ctrl+Alt+N`,
    `${zh.shortcutSearchChats}: Ctrl+Alt+K`, `${zh.shortcutOpenFolder}: Ctrl+Alt+O`, `${zh.shortcutTerminal}: Ctrl+\``,
    `${zh.shortcutSend}: Enter`, `${zh.shortcutNewline}: Shift+Enter`, `${zh.shortcutAlternate}: Ctrl+Enter`,
    `${zh.shortcutStop}: Esc+Esc`, `${zh.shortcutReload}: F5`,
  ])
})

it('exports diagnostics through the desktop and reports where the file went', async () => {
  const exported = vi.fn<() => Promise<string | null>>().mockResolvedValueOnce('C:\\Downloads\\RainyAgent-diagnostics.zip').mockResolvedValueOnce(null)
  const host = window as typeof window & { __RAINY_DIAGNOSTICS__?: { export(): Promise<string | null> } }
  host.__RAINY_DIAGNOSTICS__ = { export: exported }
  try {
    await render(dialog())
    expect(hasText(zh.diagnosticsDescription)).toBe(true)
    await click(button(zh.diagnosticsExport))
    await waitFor(() => { expect(toast).toHaveBeenCalledWith('诊断信息已保存到 C:\\Downloads\\RainyAgent-diagnostics.zip', { tone: 'success' }) })
    toast.mockClear()
    await click(button(zh.diagnosticsExport))
    await waitFor(() => { expect(exported).toHaveBeenCalledTimes(2) })
    expect(toast).not.toHaveBeenCalled()
  } finally { delete host.__RAINY_DIAGNOSTICS__ }
})

it('reports a rejected preference change', async () => {
  handle('prefs.set', () => { throw new Error('settings.json is read-only') })
  await render(dialog({ section: 'appearance' }))
  await click(button(zh.appearanceLight))
  await waitFor(() => { expect(toast).toHaveBeenCalledWith('settings.json is read-only', { tone: 'error' }) })
  expect(findButton(zh.appearanceLight)).toBeDefined()
})
