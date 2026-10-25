/** YAML (`---`) or TOML (`+++`) front matter at the start of a Markdown file. The rich editor shows it in its own box,
 *  as Typora does, instead of rendering it as a rule and a heading. */

/** A Markdown file split at the end of its front matter. */
export interface FrontMatterSplit {
  /** The whole block with its delimiter lines and the line end after the closing one; `''` when there is none. */
  readonly front: string
  /** Everything after it. */
  readonly body: string
}

const BLOCK = /^(?:---[ \t]*\r?\n(?:[\s\S]*?\r?\n)?(?:---|\.\.\.)|\+\+\+[ \t]*\r?\n(?:[\s\S]*?\r?\n)?\+\+\+)[ \t]*(?:\r?\n|$)/u

/**
 * @param text Markdown with LF or CRLF line ends.
 * @returns The front matter and the rest of the file, each with the file's own line ends.
 */
export function splitFrontMatter(text: string): FrontMatterSplit {
  const match = BLOCK.exec(text)
  return match === null ? { front: '', body: text } : { front: match[0], body: text.slice(match[0].length) }
}

/** @param front A block from {@link splitFrontMatter}. @returns Whether it is TOML (`+++`) rather than YAML. */
export function frontMatterIsToml(front: string): boolean {
  return front.startsWith('+++')
}

/**
 * @param front A block from {@link splitFrontMatter}.
 * @returns The lines between its delimiters with LF line ends, without the final line end.
 */
export function frontMatterContent(front: string): string {
  const lines = front.replace(/\r\n/gu, '\n').replace(/\n$/u, '').split('\n')
  return lines.slice(1, -1).join('\n')
}

/**
 * Replace the lines between the delimiters, keeping the delimiters and the line ends the file used.
 * @param front A block from {@link splitFrontMatter}.
 * @param content New lines between the delimiters.
 * @returns The new block, ending with a line end.
 */
export function withFrontMatterContent(front: string, content: string): string {
  const ending = front.includes('\r\n') ? '\r\n' : '\n'
  const lines = front.replace(/\r\n/gu, '\n').replace(/\n$/u, '').split('\n')
  const inner = content.replace(/\r\n?/gu, '\n').replace(/\n$/u, '')
  return `${lines[0]}\n${inner === '' ? '' : `${inner}\n`}${lines.at(-1)}\n`.replaceAll('\n', ending)
}
