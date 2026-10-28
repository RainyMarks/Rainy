/** Appearance choices the Host stores and validates and the viewer pages apply; `native/crates/app/src/theme.rs` applies
 *  them to the native interface. */

/** Color schemes of Settings → Appearance; each replaces the gray ramp in `src/renderer/theme/schemes.css`. */
export const COLOR_SCHEMES = ['graphite', 'slate', 'midnight', 'sand', 'forest', 'rose', 'black'] as const
/** One color scheme. */
export type ColorScheme = (typeof COLOR_SCHEMES)[number]

/** Accent colors; `auto` uses the color scheme's own. */
export const ACCENTS = ['auto', 'blue', 'violet', 'green', 'orange', 'pink', 'teal'] as const
/** One accent choice. */
export type Accent = (typeof ACCENTS)[number]

/** The accent each color scheme uses while the accent is `auto`. */
export const SCHEME_ACCENTS: Readonly<Record<ColorScheme, Exclude<Accent, 'auto'>>> = {
  graphite: 'blue', slate: 'blue', midnight: 'violet', sand: 'orange', forest: 'green', rose: 'pink', black: 'blue',
}

/**
 * @param scheme Color scheme.
 * @param accent Accent choice.
 * @returns The accent drawn.
 */
export function effectiveAccent(scheme: ColorScheme, accent: Accent): Exclude<Accent, 'auto'> {
  return accent === 'auto' ? SCHEME_ACCENTS[scheme] : accent
}

/** Longest stored font setting, in characters. */
const FONT_SETTING_MAX = 160
/** Most families one font setting may list. */
const FONT_FAMILIES_MAX = 4
/** Letters of any script, digits, spaces and the few marks real family names use; no quotes, commas or semicolons. */
const FONT_NAME = /^[\p{L}\p{M}\p{N} ._+&()-]{1,64}$/u
/** CSS generic families, written without quotes. */
const GENERIC_FAMILIES = new Set(['serif', 'sans-serif', 'monospace', 'cursive', 'fantasy', 'system-ui', 'ui-serif', 'ui-sans-serif',
  'ui-monospace', 'ui-rounded', 'math', 'emoji', 'fangsong'])

/**
 * @param value A font setting: empty for the default, or family names separated by commas, each optionally quoted.
 * @returns The family names, or `undefined` when the setting is not valid.
 */
function fontFamilies(value: string): string[] | undefined {
  if (value.length > FONT_SETTING_MAX) return undefined
  if (value.trim() === '') return []
  const names = value.split(',').map(name => name.trim().replace(/^(['"])(.*)\1$/u, '$2').trim())
  if (names.length > FONT_FAMILIES_MAX || names.some(name => !FONT_NAME.test(name))) return undefined
  return names
}

/**
 * @param value A font setting.
 * @returns The families as the start of a CSS `font-family` list, each followed by a comma; empty for the default or an
 * invalid setting.
 */
export function fontListPrefix(value: string): string {
  return (fontFamilies(value) ?? []).map(name => GENERIC_FAMILIES.has(name.toLowerCase()) ? `${name},` : `"${name}",`).join(' ')
}
