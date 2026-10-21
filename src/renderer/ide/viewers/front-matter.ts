/** YAML (`---`) or TOML (`+++`) front matter at the start of a Markdown file. The rich editor shows it in its own box, as
 *  Typora does, instead of rendering it as a rule and a heading. */

/** A Markdown file split at the end of its front matter. */
export interface FrontMatterSplit {
  /** The whole block with its delimiter lines and the line end after the closing one; `''` when there is none. */
  readonly front: string
  /** Everything after it. */
  readonly body: string
}

const BLOCK = /^(?:---[ \t]*\n(?:[\s\S]*?\n)?(?:---|\.\.\.)|\+\+\+[ \t]*\n(?:[\s\S]*?\n)?\+\+\+)[ \t]*(?:\n|$)/u

/**
 * @param text Markdown with LF line ends, as the editor holds it.
 * @returns The front matter and the rest of the file.
 */
export function splitFrontMatter(text: string): FrontMatterSplit {
  const match = BLOCK.exec(text)
  return match === null ? { front: '', body: text } : { front: match[0], body: text.slice(match[0].length) }
}

/**
 * @param front A block from {@link splitFrontMatter}.
 * @returns The lines between its delimiters, without the final line end.
 */
export function frontMatterContent(front: string): string {
  const lines = front.replace(/\n$/u, '').split('\n')
  return lines.slice(1, -1).join('\n')
}

/**
 * Replace the lines between the delimiters, keeping the delimiters the file used.
 * @param front A block from {@link splitFrontMatter}.
 * @param content New lines between the delimiters.
 * @returns The new block, ending with a line end.
 */
export function withFrontMatterContent(front: string, content: string): string {
  const lines = front.replace(/\n$/u, '').split('\n')
  const inner = content.replace(/\r\n?/gu, '\n').replace(/\n$/u, '')
  return `${lines[0]}\n${inner === '' ? '' : `${inner}\n`}${lines.at(-1)}\n`
}
