/** Chinese and English UI strings, chosen by the locale preference. */
import { useCallback } from 'react'
import { getPrefs, usePrefs } from './prefs.ts'

/** Supported interface languages. */
export type Locale = 'zh' | 'en'
/** Values substituted for `{name}` placeholders. */
export type MessageVars = Readonly<Record<string, string | number>>
/** Looks up one message. */
export type Translate<K extends string> = (key: K, vars?: MessageVars) => string

function format(template: string, vars: MessageVars | undefined): string {
  if (vars === undefined) return template
  return template.replace(/\{(\w+)\}/g, (match, name: string) => name in vars ? String(vars[name]) : match)
}

/**
 * Declare one module's messages in both languages.
 * @param zh Chinese strings; the keys define the module's message set.
 * @param en English strings for the same keys.
 * @returns A hook returning the translate function for the current locale, and a non-hook lookup.
 */
export function defineMessages<K extends string>(zh: Readonly<Record<K, string>>, en: Readonly<Record<K, string>>): {
  useT(): Translate<K>
  t: Translate<K>
} {
  const tables: Readonly<Record<Locale, Readonly<Record<K, string>>>> = { zh, en }
  const t: Translate<K> = (key, vars) => format(tables[getPrefs().locale][key], vars)
  return {
    useT() {
      const { locale } = usePrefs()
      return useCallback<Translate<K>>((key, vars) => format(tables[locale][key], vars), [locale])
    },
    t,
  }
}

/** @returns The current interface language; re-renders when it changes. */
export function useLocale(): Locale {
  return usePrefs().locale
}
