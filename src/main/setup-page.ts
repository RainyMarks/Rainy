/** Setup windows load static pages from `src/setup`; `src/setup/locale.js` switches them to English for `lang=en`. */
import { pathToFileURL } from 'node:url'
import { currentLocale } from '../shared/locale.ts'

/**
 * @param path Page file.
 * @returns Its file URL in the current interface language; the preload compares the page against exactly this URL.
 */
export function setupPageURL(path: string): string {
  const url = pathToFileURL(path)
  if (currentLocale() === 'en') url.searchParams.set('lang', 'en')
  return url.href
}
