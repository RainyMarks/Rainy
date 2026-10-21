/** Word and character counts of a Markdown document, as a reader sees it: syntax, link targets and front matter are left out. */
import { splitFrontMatter } from './front-matter.ts'

/** Counts shown in the status bar. */
export interface WordCount {
  /** Chinese, Japanese and Korean characters each count as a word, like in Word; other scripts count words. */
  readonly words: number
  /** Characters other than spaces. */
  readonly characters: number
  /** Minutes a reader needs: 300 CJK characters or 200 other words a minute, at least one for any text. */
  readonly minutes: number
}

const CJK = /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}]/gu

/**
 * @param markdown Document text.
 * @returns The text without Markdown syntax, link targets, HTML tags and front matter.
 */
export function readableText(markdown: string): string {
  return splitFrontMatter(markdown).body
    .replace(/^ {0,3}(?:`{3,}|~{3,}).*$/gmu, '')
    .replace(/^ {0,3}\$\$[ \t]*$/gmu, '')
    .replace(/!\[([^\]]*)\]\([^)]*\)/gu, '$1')
    .replace(/\[([^\]]*)\]\([^)]*\)/gu, '$1')
    .replace(/^ {0,3}\[[^\]]+\]:.*$/gmu, '')
    .replace(/<[^>]+>/gu, ' ')
    .replace(/^(?:[ \t]*(?:>|[-*+](?=[ \t])|\d+[.)](?=[ \t])|#{1,6}(?=[ \t]|$)|\[[ xX]\](?=[ \t])))+/gmu, '')
    .replace(/^ {0,3}(?:[-=_*][ \t]*){3,}$/gmu, '')
    .replace(/^[ \t]*\|?(?:[ \t]*:?-+:?[ \t]*\|)+[ \t]*:?-*:?[ \t]*$/gmu, '')
    .replace(/[*_~`|#>\\]/gu, ' ')
}

/**
 * @param markdown Document text.
 * @returns Its counts.
 */
export function countWords(markdown: string): WordCount {
  const text = readableText(markdown)
  const cjk = text.match(CJK)?.length ?? 0
  const other = text.replace(CJK, ' ').match(/[\p{L}\p{N}][\p{L}\p{N}\p{M}'’_.-]*/gu)?.length ?? 0
  let characters = 0
  for (const character of text) if (!/\s/u.test(character)) characters++
  const words = cjk + other
  return { words, characters, minutes: words === 0 ? 0 : Math.max(1, Math.round(cjk / 300 + other / 200)) }
}
