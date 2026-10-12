/**
 * Messages the Host and the desktop process show, in the interface language the user chose. The page itself uses the
 * message tables in each `messages.ts`; these processes carry both texts where a message is written.
 */

/** Interface language. */
export type Locale = 'zh' | 'en'

/** One message in both interface languages. */
export interface Bilingual {
  readonly zh: string
  readonly en: string
}

let current: () => Locale = () => 'zh'

/**
 * Choose where this process reads the interface language.
 * @param read Returns the language messages are written in from now on.
 */
export function followLocale(read: () => Locale): void {
  current = read
}

/** @returns The interface language this process writes messages in. */
export function currentLocale(): Locale {
  return current()
}

/**
 * @param text The message in both languages.
 * @returns The message in the interface language.
 */
export function localized(text: Bilingual): string {
  return text[current()]
}
