/** Interface preferences stored by the Host, with theme and font sizes applied to the page. */
import { useSyncExternalStore } from 'react'
import type { UiPreferences } from '../shared/rpc.ts'
import { host } from './rpc.ts'

/** Defaults until the Host answers; they match the Host's own defaults. */
const DEFAULT_PREFS: UiPreferences = {
  locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true,
}

let current: UiPreferences = DEFAULT_PREFS
const listeners = new Set<() => void>()
const darkQuery = typeof window === 'undefined' ? undefined : window.matchMedia('(prefers-color-scheme: dark)')

function apply(prefs: UiPreferences): void {
  if (typeof document === 'undefined') return
  const dark = prefs.theme === 'dark' || (prefs.theme === 'system' && darkQuery?.matches === true)
  document.body.toggleAttribute('data-ds-dark-theme', dark)
  document.documentElement.lang = prefs.locale === 'zh' ? 'zh-CN' : 'en'
  document.documentElement.style.setProperty('--dsh-content-font-size', `${prefs.uiFontSize}px`)
  document.documentElement.style.setProperty('--dsh-code-font-size', `${prefs.codeFontSize}px`)
}

function publish(prefs: UiPreferences): void {
  current = prefs
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
 * Change preferences; the page updates as soon as the Host confirms.
 * @param change Fields to change.
 */
export async function setPrefs(change: Partial<UiPreferences>): Promise<void> {
  publish({ ...current, ...change })
  publish(await host.call('prefs.set', change))
}

/** @returns Whether the page currently uses the dark palette. */
export function isDark(): boolean {
  return typeof document !== 'undefined' && document.body.hasAttribute('data-ds-dark-theme')
}
