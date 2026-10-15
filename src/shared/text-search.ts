/** Text search across a project's files, and the replacement the Host applies on disk and the page in unsaved buffers. */
import type { IdeRootId, WorkspaceId } from './ide-files-protocol.ts'

/** What to look for. */
export interface TextSearchOptions {
  readonly pattern: string
  /** `pattern` is a regular expression rather than literal text. */
  readonly regex: boolean
  readonly caseSensitive: boolean
  readonly wholeWord: boolean
  /** Comma-separated globs a file must match (`src/**, *.py`); empty for every file. */
  readonly include: string
  /** Comma-separated globs of files to leave out. */
  readonly exclude: string
}

/** A search over every root of a project. */
export interface TextSearchRequest extends TextSearchOptions {
  readonly workspaceId: WorkspaceId
}

/** One match. Searches are line by line, so a match never spans lines. */
export interface TextSearchMatch {
  /** One-based line. */
  readonly line: number
  /** Zero-based UTF-16 column in the line. */
  readonly column: number
  readonly length: number
  /** The line, or the part of a long line around the match. */
  readonly preview: string
  /** Where the match starts in `preview`. */
  readonly previewColumn: number
}

/** The matches in one file. */
export interface TextSearchFile {
  readonly rootId: IdeRootId
  /** Root-relative path with `/` separators. */
  readonly path: string
  readonly matches: readonly TextSearchMatch[]
}

/** Matches grouped by file, in path order. */
export interface TextSearchResult {
  readonly files: readonly TextSearchFile[]
  /** Matches across all files. */
  readonly matches: number
  /** The search stopped at its limit or deadline; more files may match. */
  readonly truncated: boolean
}

/** Replace every match in the listed files on disk. */
export interface TextReplaceRequest extends TextSearchRequest {
  readonly replacement: string
  readonly files: readonly { readonly rootId: IdeRootId; readonly path: string }[]
}

/** What a replacement changed. */
export interface TextReplaceResult {
  readonly files: readonly { readonly rootId: IdeRootId; readonly path: string; readonly replaced: number }[]
  readonly failed: readonly { readonly rootId: IdeRootId; readonly path: string; readonly message: string }[]
}

/** Lines longer than this are previewed around their match. */
export const PREVIEW_CHARS = 240
/** Characters kept before a match in a cut preview. */
const PREVIEW_BEFORE = 40

/**
 * Build the expression a search runs, for replacing and highlighting.
 * @param options Pattern and flags.
 * @returns A global expression; throws a `SyntaxError` when the pattern is not a valid expression.
 */
export function searchExpression(options: Pick<TextSearchOptions, 'pattern' | 'regex' | 'caseSensitive' | 'wholeWord'>): RegExp {
  let source = options.regex ? options.pattern : options.pattern.replace(/[.*+?^${}()|[\]\\/]/gu, '\\$&')
  if (options.wholeWord) source = `\\b(?:${source})\\b`
  const flags = options.caseSensitive ? 'g' : 'gi'
  try {
    return new RegExp(source, `${flags}u`)
  } catch (_notUnicodeSafe) {
    // Patterns written for the non-Unicode dialect (an escaped `-`, say) still work without the flag.
    return new RegExp(source, flags)
  }
}

/**
 * Expand the escapes a regular-expression replacement may use: `\n`, `\t` and `\\`.
 * @param replacement Typed replacement.
 * @returns The replacement with the escapes applied; `$1` and `$&` stay for `String.replace`.
 */
function expandEscapes(replacement: string): string {
  return replacement.replace(/\\([nt\\])/gu, (_match, char: string) => char === 'n' ? '\n' : char === 't' ? '\t' : '\\')
}

/**
 * Replace every match line by line, as the search finds them.
 * @param text File text.
 * @param options Pattern and flags.
 * @param replacement Literal text; with `regex`, `$1`, `$&`, `\n` and `\t` are expanded.
 * @returns The new text and how many matches were replaced.
 */
export function replaceInText(text: string, options: Pick<TextSearchOptions, 'pattern' | 'regex' | 'caseSensitive' | 'wholeWord'>, replacement: string): { text: string; count: number } {
  if (options.pattern === '') return { text, count: 0 }
  const expression = searchExpression(options)
  const expanded = options.regex ? expandEscapes(replacement) : replacement
  let count = 0
  const parts = text.split(/(\r\n|\n|\r)/u)
  for (let index = 0; index < parts.length; index += 2) {
    const line = parts[index]!
    const found = [...line.matchAll(expression)].length
    if (found === 0) continue
    count += found
    // A literal replacement goes in as typed; a pattern's replacement expands `$1` and `$&` as `String.replace` does.
    parts[index] = options.regex ? line.replace(expression, expanded) : line.replace(expression, () => expanded)
  }
  return { text: parts.join(''), count }
}

/**
 * Cut a long line to the part around a match.
 * @param line Whole line.
 * @param column Match start.
 * @returns The preview and where the match starts in it.
 */
export function previewLine(line: string, column: number): { preview: string; previewColumn: number } {
  if (line.length <= PREVIEW_CHARS) return { preview: line, previewColumn: column }
  const start = Math.max(0, column - PREVIEW_BEFORE)
  const preview = (start > 0 ? '…' : '') + line.slice(start, start + PREVIEW_CHARS) + (start + PREVIEW_CHARS < line.length ? '…' : '')
  return { preview, previewColumn: column - start + (start > 0 ? 1 : 0) }
}

/**
 * Split a glob list as typed.
 * @param list Comma-separated globs.
 * @returns The globs, trimmed, without empty entries.
 */
export function globList(list: string): string[] {
  return list.split(',').map(glob => glob.trim()).filter(Boolean)
}
