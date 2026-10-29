/** Keep a Markdown file's own spelling when a rich editor rewrites it: only the lines the user changed take the editor's
 *  serialization; every other line stays exactly as it was in the file, line ends included. */
import { diffArrays } from 'diff'

/**
 * Write table delimiter rows as `| --- |`, the usual style, instead of the minimal `| - |` an unaligned table gets;
 * lines inside fenced code are left alone.
 * @param markdown Serialized document.
 * @returns The same document with conventional delimiter rows.
 */
export function conventionalTables(markdown: string): string {
  let fence: string | undefined
  return markdown.split('\n').map((line) => {
    const opening = /^\s{0,3}(`{3,}|~{3,})/u.exec(line)?.[1]
    if (opening !== undefined) {
      if (fence === undefined) fence = opening
      else if (opening[0] === fence[0] && opening.length >= fence.length) fence = undefined
      return line
    }
    if (fence !== undefined || !/^\|(?:\s*:?-+:?\s*\|)+\s*$/u.test(line)) return line
    return line.replace(/(\|)\s*(:?)-+(:?)\s*(?=\|)/gu, (_match, pipe: string, left: string, right: string) => `${pipe} ${left}---${right} `)
  }).join('\n')
}

/** A stretch of the editor's serialization of the file and the file lines it corresponds to. */
interface Block { loadedStart: number; loadedEnd: number; sourceStart: number; sourceEnd: number }

/** A file's lines without their ends, and the end after each (`''` after the last). */
interface Lines { readonly text: string[]; readonly ends: string[] }

/** @param text File text with any line ends. @returns Its lines and their ends: `\r\n`, `\n` or a lone `\r`. */
export function splitLines(text: string): Lines {
  const parts = text.split(/(\r\n|\n|\r)/u)
  const lines: string[] = []
  const ends: string[] = []
  for (let index = 0; index < parts.length; index += 2) {
    lines.push(parts[index]!)
    ends.push(parts[index + 1] ?? '')
  }
  return { text: lines, ends }
}

/** @param text File text. @returns The line end most of its lines use; `\n` when it has none. */
export function lineEnding(text: string): string {
  const counts = new Map<string, number>()
  for (const end of splitLines(text).ends) if (end !== '') counts.set(end, (counts.get(end) ?? 0) + 1)
  let best = '\n'
  let most = 0
  for (const [end, count] of counts) if (count > most) { best = end; most = count }
  return best
}

/** @param text Text with LF line ends. @param ending The line end to write. @returns The text with that line end. */
export function withLineEnding(text: string, ending: string): string {
  return ending === '\n' ? text : text.replaceAll('\n', ending)
}

/** Pair the serialization of the unchanged document with the file: equal lines one by one, differing stretches as a whole. */
function blocks(loaded: readonly string[], source: readonly string[]): Block[] {
  const result: Block[] = []
  let at = 0
  let from = 0
  let pending: Block | undefined
  const flush = (): void => { if (pending !== undefined) { result.push(pending); pending = undefined } }
  for (const part of diffArrays(loaded as string[], source as string[])) {
    const count = part.count ?? part.value.length
    if (!part.added && !part.removed) {
      flush()
      for (let index = 0; index < count; index++) result.push({ loadedStart: at + index, loadedEnd: at + index + 1, sourceStart: from + index, sourceEnd: from + index + 1 })
      at += count
      from += count
      continue
    }
    pending ??= { loadedStart: at, loadedEnd: at, sourceStart: from, sourceEnd: from }
    if (part.removed) { at += count; pending.loadedEnd = at } else { from += count; pending.sourceEnd = from }
  }
  flush()
  return result
}

/**
 * @param source The file text the editor was loaded with, with its own line ends (CRLF, LF or mixed).
 * @param loaded The editor's serialization right after loading `source`, with LF line ends.
 * @param edited The editor's serialization now, with LF line ends.
 * @returns `source` with the user's changes: stretches the edit did not touch keep their original lines and line ends;
 *   new lines take the line end most of the file uses.
 */
export function preserveSource(source: string, loaded: string, edited: string): string {
  if (edited === loaded) return source
  const before = loaded.split('\n')
  const original = splitLines(source)
  const ending = lineEnding(source)
  // For each line of `loaded`: kept by the edit or not, and the edited lines inserted in front of it.
  const kept = new Array<boolean>(before.length).fill(true)
  const inserted: string[][] = Array.from({ length: before.length + 1 }, () => [])
  let at = 0
  for (const part of diffArrays(before, edited.split('\n'))) {
    const count = part.count ?? part.value.length
    if (part.added) inserted[at]!.push(...part.value)
    else if (part.removed) { for (let index = 0; index < count; index++) kept[at + index] = false; at += count } else at += count
  }
  /** Output lines; `end` is the file's own line end of a line kept from it. */
  const out: { text: string; end?: string | undefined }[] = []
  const placed = new Set<number>()
  const insertAt = (index: number): void => {
    if (placed.has(index)) return
    placed.add(index)
    for (const text of inserted[index]!) out.push({ text })
  }
  for (const block of blocks(before, original.text)) {
    insertAt(block.loadedStart)
    let touched = false
    for (let index = block.loadedStart; index < block.loadedEnd; index++) {
      if (!kept[index] || (index > block.loadedStart && inserted[index]!.length > 0)) { touched = true; break }
    }
    if (!touched) {
      for (let index = block.sourceStart; index < block.sourceEnd; index++) out.push({ text: original.text[index]!, end: original.ends[index] })
      continue
    }
    for (let index = block.loadedStart; index < block.loadedEnd; index++) {
      if (index > block.loadedStart) insertAt(index)
      if (kept[index]) out.push({ text: before[index]! })
    }
  }
  insertAt(before.length)
  return out.map((line, index) => index === out.length - 1 ? line.text : line.text + (line.end === undefined || line.end === '' ? ending : line.end)).join('')
}

/** Where a top-level block of a Markdown text starts and ends, as offsets into it. */
export type BlockRange = readonly [start: number, end: number]

/**
 * The coarser fallback: whole top-level blocks (paragraphs, lists, tables…) the edit did not change keep their text from
 * the file, changed and new blocks take the editor's.
 * @param source The file text the editor was loaded with, LF line ends.
 * @param loaded The editor's serialization right after loading it.
 * @param edited The editor's serialization now.
 * @param ranges Top-level blocks of a text, from the editor's own Markdown parser.
 * @returns The merged text, or `undefined` when the file's blocks do not pair with the editor's.
 */
export function preserveBlocks(source: string, loaded: string, edited: string, ranges: (text: string) => readonly BlockRange[]): string | undefined {
  if (edited === loaded) return source
  const from = ranges(source)
  const was = ranges(loaded)
  const now = ranges(edited)
  if (from.length !== was.length || from.length === 0) return undefined
  const slice = (text: string, list: readonly BlockRange[]): string[] => list.map(([start, end]) => text.slice(start, end))
  const pieces: { text: string; source?: number | undefined; edited?: number | undefined }[] = []
  let at = 0
  let to = 0
  for (const part of diffArrays(slice(loaded, was), slice(edited, now))) {
    const count = part.count ?? part.value.length
    if (part.removed) { at += count; continue }
    if (part.added) { for (let index = 0; index < count; index++) pieces.push({ text: part.value[index]!, edited: to + index }); to += count; continue }
    for (let index = 0; index < count; index++) pieces.push({ text: source.slice(from[at + index]![0], from[at + index]![1]), source: at + index })
    at += count
    to += count
  }
  if (pieces.length === 0) return edited
  let out = pieces[0]!.source === 0 ? source.slice(0, from[0]![0]) : ''
  for (const [index, piece] of pieces.entries()) {
    const previous = pieces[index - 1]
    if (previous !== undefined) {
      // The file's own gap between two of its blocks that stay neighbours, else the editor's.
      if (previous.source !== undefined && piece.source === previous.source + 1) out += source.slice(from[previous.source]![1], from[piece.source]![0])
      else if (piece.edited !== undefined && piece.edited > 0) out += edited.slice(now[piece.edited - 1]![1], now[piece.edited]![0])
      else out += '\n\n'
    }
    out += piece.text
  }
  const last = pieces.at(-1)!
  return out + (last.source === from.length - 1 ? source.slice(from.at(-1)![1]) : '\n')
}

/** Lines `[start, end)` of a base text replaced by `lines`. */
interface Hunk { readonly start: number; readonly end: number; readonly lines: readonly string[] }

/** @returns The stretches of `base` that `other` changed, in order. */
function hunks(base: readonly string[], other: readonly string[]): Hunk[] {
  const result: Hunk[] = []
  let at = 0
  let open: { start: number; end: number; lines: string[] } | undefined
  for (const part of diffArrays(base as string[], other as string[])) {
    const count = part.count ?? part.value.length
    if (!part.added && !part.removed) {
      if (open !== undefined) { result.push(open); open = undefined }
      at += count
      continue
    }
    open ??= { start: at, end: at, lines: [] }
    if (part.removed) { at += count; open.end = at } else open.lines.push(...part.value)
  }
  if (open !== undefined) result.push(open)
  return result
}

/**
 * Carry an edit over onto a text that changed meanwhile, line by line, as diff3 does: `mine` and `theirs` both started
 * from `base`.
 * @param base The text both started from.
 * @param mine The edited text.
 * @param theirs The text it changed into meanwhile.
 * @returns `theirs` with the lines `mine` changed, or `undefined` when both changed the same or neighbouring lines.
 */
export function rebaseEdit(base: string, mine: string, theirs: string): string | undefined {
  if (mine === base || mine === theirs) return theirs
  if (theirs === base) return mine
  const lines = base.split('\n')
  const ours = hunks(lines, mine.split('\n'))
  const others = hunks(lines, theirs.split('\n'))
  if (ours.some(a => others.some(b => a.start <= b.end && b.start <= a.end))) return undefined
  const out: string[] = []
  let at = 0
  for (const hunk of [...ours, ...others].sort((a, b) => a.start - b.start)) {
    out.push(...lines.slice(at, hunk.start), ...hunk.lines)
    at = hunk.end
  }
  out.push(...lines.slice(at))
  return out.join('\n')
}

/** What the merge needs from the editor's own Markdown parser and serializer. */
export interface MarkdownReader {
  /** @returns The editor's serialization of its parse of `text`. */
  canonical(text: string): string
  /** @returns The top-level blocks of `text` (LF line ends). */
  ranges(text: string): readonly BlockRange[]
}

/**
 * The file with the user's edit: merged line by line, else block by block, and only when the merged text reads back as
 * the document the editor holds; otherwise the editor's own text, which loses spelling but never content.
 * @param source The file text the editor was loaded with, with its own line ends.
 * @param loaded The editor's serialization right after loading it.
 * @param written The editor's serialization now.
 * @param reader The editor's parser and serializer.
 * @returns The text to save, with the file's line ends.
 */
export function mergeEdit(source: string, loaded: string, written: string, reader: MarkdownReader): string {
  if (written === loaded) return source
  const ending = lineEnding(source)
  let wanted: string | undefined
  const holds = (merged: string | undefined): merged is string => {
    if (merged === undefined) return false
    try { return reader.canonical(merged) === (wanted ??= reader.canonical(written)) } catch (_unparsable) { return false }
  }
  const lines = preserveSource(source, loaded, written)
  if (holds(lines)) return lines
  const blocks = preserveBlocks(source.replace(/\r\n?/gu, '\n'), loaded, written, reader.ranges)
  if (holds(blocks)) return withLineEnding(blocks, ending)
  return withLineEnding(written, ending)
}
