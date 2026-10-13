/** Interface preferences stored by the Host, with theme and font sizes applied to the page. */
import { useSyncExternalStore } from 'react'
import type { UiPreferences } from '../shared/rpc.ts'
import { host } from './rpc.ts'

/** Defaults until the Host answers; they match the Host's own defaults. */
const DEFAULT_PREFS: UiPreferences = {
  locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true,
}

let current: UiPreferences = DEFAULT_PREFS
/** Whether `current` holds the user's preferences rather than the defaults shown before the Host answered. */
let known = false
const listeners = new Set<() => void>()
const darkQuery = typeof window === 'undefined' ? undefined : window.matchMedia('(prefers-color-scheme: dark)')

function apply(prefs: UiPreferences): void {
  if (typeof document === 'undefined') return
  const dark = prefs.theme === 'dark' || (prefs.theme === 'system' && darkQuery?.matches === true)
  document.body.toggleAttribute('data-ds-dark-theme', dark)
  document.documentElement.lang = prefs.locale === 'zh' ? 'zh-CN' : 'en'
  document.documentElement.style.setProperty('--dsh-content-font-size', `${prefs.uiFontSize}px`)
  document.documentElement.style.setProperty('--dsh-code-font-size', `${prefs.codeFontSize}px`)
  // The desktop themes native menus and dialogs from this attribute, and takes the page language as the user's only
  // while it is present (`src/preload/preload-chrome.ts`), so the defaults never reach it.
  if (known) document.documentElement.setAttribute('data-ds-theme-source', prefs.theme)
}

function publish(prefs: UiPreferences): void {
  current = prefs
  known = true
  apply(prefs)
  for (const listener of listeners) listener()
}

if (typeof window !== 'undefined') {
  apply(current)
  darkQuery?.addEventListener('change', () => { apply(current) })
  host.on('prefs.changed', publish)
  host.onState(() => { if (host.state === 'open') void host.call('prefs.get').then(publish, () => undefined) })
  void host.call('prefs.get').then(publish, () => undefined)
}

/** @returns The current preferences; re-renders when they change. */
export function usePrefs(): UiPreferences {
  return useSyncExternalStore(listener => { listeners.add(listener); return () => { listeners.delete(listener) } }, () => current)
}

/** @returns The current preferences without subscribing. */
export function getPrefs(): UiPreferences {
  return current
}

/**
 * Change preferences. The page shows the change at once and keeps the Host's answer; a rejected change is undone.
 * @param change Fields to change.
 * @returns Completion; rejects with the Host's error after restoring the previous preferences.
 */
export async function setPrefs(change: Partial<UiPreferences>): Promise<void> {
  const previous = current
  publish({ ...current, ...change })
  try {
    publish(await host.call('prefs.set', change))
  } catch (error) {
    publish(previous)
    throw error
  }
}

/** @returns Whether the page currently uses the dark palette. */
export function isDark(): boolean {
  return typeof document !== 'undefined' && document.body.hasAttribute('data-ds-dark-theme')
}
