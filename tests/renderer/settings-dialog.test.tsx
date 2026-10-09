// @vitest-environment happy-dom
/** The settings window opens on the requested section, closes on Escape, and stores General preferences through the Host. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { UiPreferences } from '../../src/shared/rpc.ts'
import { SettingsDialog } from '../../src/renderer/settings/SettingsDialog.tsx'
import type { SettingsDialogProps } from '../../src/renderer/settings/SettingsDialog.tsx'
import { settingsMessages } from '../../src/renderer/settings/messages.ts'
import { getPrefs } from '../../src/renderer/prefs.ts'
import { act } from 'react'
import { all, button, cleanup, click, emit, fakeHost, findButton, handle, hasText, render, toast, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const zh = settingsMessages.zh
const en = settingsMessages.en
const defaults: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true }
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
    .toEqual([zh.navGeneral, zh.navModels, zh.navExtensions, zh.navRuntime, zh.navMemory])
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
  await click(button(zh.languageTitle))
  await click(all<HTMLElement>('[role="menuitem"]').find(item => item.textContent === 'English')!)
  await waitFor(() => { expect(stored.locale).toBe('en') })
  expect(hasText(en.navGeneral)).toBe(true)
  expect(getPrefs().locale).toBe('en')

  await click(button(en.appearanceDark))
  expect(stored.theme).toBe('dark')
  expect(button(en.appearanceDark).getAttribute('aria-pressed')).toBe('true')
  expect(button(en.appearanceSystem).getAttribute('aria-pressed')).toBe('false')

  await click(button(en.busyEnterTitle))
  await click(all<HTMLElement>('[role="menuitem"]').find(item => item.textContent === en.busyEnterSteer)!)
  expect(stored.busyEnter).toBe('steer')
  await click(button(en.stepDetailTitle))
  await click(all<HTMLElement>('[role="menuitem"]').find(item => item.textContent === en.stepDetailDetailed)!)
  expect(stored.stepDetail).toBe('detailed')
  await click(button(en.usageTitle))
  expect(stored.showUsage).toBe(false)
  expect(button(en.usageTitle).getAttribute('aria-checked')).toBe('false')
  expect(fakeHost.call.mock.calls.filter(([method]) => method === 'prefs.set').map(([, params]) => params)).toEqual([
    { locale: 'en' }, { theme: 'dark' }, { busyEnter: 'steer' }, { stepDetail: 'detailed' }, { showUsage: false },
  ])
})

it('steps font sizes within 12 to 17 px and resets both to the defaults', async () => {
  await emit('prefs.changed', { ...stored, uiFontSize: 16, codeFontSize: 12 })
  stored = { ...stored, uiFontSize: 16, codeFontSize: 12 }
  await render(dialog())
  const value = (field: string): string | null | undefined => document.querySelector(`[data-font-size="${field}"]`)?.textContent
  expect(button(zh.codeFontSizeDecrease).disabled).toBe(true)
  await click(button(zh.fontSizeIncrease))
  expect(value('uiFontSize')).toBe('17')
  expect(button(zh.fontSizeIncrease).disabled).toBe(true)
  expect(document.documentElement.style.getPropertyValue('--dsh-content-font-size')).toBe('17px')
  await click(button(zh.codeFontSizeIncrease))
  expect(value('codeFontSize')).toBe('13')
  await click(button(zh.fontSizeReset))
  expect(stored).toMatchObject({ uiFontSize: 14, codeFontSize: 13 })
  expect(button(zh.fontSizeReset).disabled).toBe(true)
})

it('lists the keyboard shortcuts and the current version', async () => {
  window.__RAINY_AGENT__ = { name: 'RainyAgent', version: '2.0.0', environment: 'Windows' }
  await render(dialog())
  const rows = all('[data-shortcuts] dt').map(row => `${row.textContent}: ${[...row.nextElementSibling!.querySelectorAll('kbd')].map(key => key.textContent).join('+')}`)
  expect(rows).toEqual([
    `${zh.shortcutQuickOpen}: Ctrl+Alt+P`, `${zh.shortcutSettings}: Ctrl+,`, `${zh.shortcutNewChat}: Ctrl+Alt+N`,
    `${zh.shortcutSearchChats}: Ctrl+Alt+K`, `${zh.shortcutOpenFolder}: Ctrl+Alt+O`, `${zh.shortcutTerminal}: Ctrl+\``,
    `${zh.shortcutSend}: Enter`, `${zh.shortcutNewline}: Shift+Enter`, `${zh.shortcutAlternate}: Ctrl+Enter`,
    `${zh.shortcutStop}: Esc+Esc`, `${zh.shortcutReload}: F5`,
  ])
  expect(hasText('当前版本：2.0.0')).toBe(true)
})

it('reports a rejected preference change', async () => {
  handle('prefs.set', () => { throw new Error('settings.json is read-only') })
  await render(dialog())
  await click(button(zh.appearanceLight))
  await waitFor(() => { expect(toast).toHaveBeenCalledWith('settings.json is read-only') })
  expect(findButton(zh.appearanceLight)).toBeDefined()
})
