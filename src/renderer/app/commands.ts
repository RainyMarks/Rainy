/** Commands of the command palette and how a typed query ranks them. */

/** One command the palette can run. */
export interface PaletteCommand {
  readonly id: string
  readonly label: string
  /** Group named in front of the label, such as "View". */
  readonly category: string
  /** Key caps of its shortcut, as `ShortcutKeys` shows them. */
  readonly keys?: readonly string[] | undefined
  readonly disabled?: boolean | undefined
  readonly run: () => void
}

/** Commands run lately, most recent first. */
const RECENT_KEY = 'rainy.palette.recent'
const RECENT_MAX = 8

/**
 * Score how well a query matches a text: every query character must appear in order. Consecutive characters and
 * characters at the start of words count more.
 * @param query Typed text, already lower case.
 * @param text Command text.
 * @returns A score, higher is better, or `undefined` when the text does not match.
 */
export function matchScore(query: string, text: string): number | undefined {
  if (query === '') return 0
  const haystack = text.toLowerCase()
  const direct = haystack.indexOf(query)
  if (direct >= 0) return 1000 - direct + (direct === 0 || /[\s:/(·]/u.test(haystack[direct - 1]!) ? 200 : 0)
  let score = 0
  let position = 0
  let previous = -2
  for (const char of query) {
    if (char === ' ') continue
    const found = haystack.indexOf(char, position)
    if (found < 0) return undefined
    score += found === previous + 1 ? 8 : 1
    if (found === 0 || /[\s:/(·]/u.test(haystack[found - 1]!)) score += 6
    previous = found
    position = found + 1
  }
  return score
}

/**
 * Order commands for a query: matches by score, recently run ones first on an empty query.
 * @param commands Every command.
 * @param query Typed text.
 * @param recent Command ids run lately, most recent first.
 * @returns The matching commands.
 */
export function rankCommands(commands: readonly PaletteCommand[], query: string, recent: readonly string[]): PaletteCommand[] {
  const needle = query.trim().toLowerCase()
  const order = new Map(recent.map((id, index) => [id, index]))
  const scored = commands.flatMap((command) => {
    const score = matchScore(needle, `${command.category}: ${command.label}`) ?? matchScore(needle, command.label)
    return score === undefined ? [] : [{ command, score }]
  })
  return scored.sort((left, right) => {
    if (needle === '') {
      const a = order.get(left.command.id) ?? Infinity
      const b = order.get(right.command.id) ?? Infinity
      if (a !== b) return a - b
      return 0
    }
    return right.score - left.score
  }).map(entry => entry.command)
}

/** @returns Command ids run lately, most recent first. */
export function recentCommands(): string[] {
  try {
    const stored = JSON.parse(localStorage.getItem(RECENT_KEY) ?? '[]') as unknown
    return Array.isArray(stored) ? stored.filter((id): id is string => typeof id === 'string').slice(0, RECENT_MAX) : []
  } catch (_unavailable) {
    return []
  }
}

/** @param id A command just run, remembered first. */
export function rememberCommand(id: string): void {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify([id, ...recentCommands().filter(other => other !== id)].slice(0, RECENT_MAX)))
  } catch (_unavailable) { /* Without storage the palette simply keeps its own order. */ }
}
