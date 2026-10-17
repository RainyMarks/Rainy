/** Keep a Markdown file's own spelling when a rich editor rewrites it: only the lines the user changed take the editor's
 *  serialization; every other line stays exactly as it was in the file. */
import { diffArrays } from 'diff'

/** A stretch of the editor's serialization of the file and the file lines it corresponds to. */
interface Block { loadedStart: number; loadedEnd: number; sourceStart: number; sourceEnd: number }

function lines(text: string): string[] {
  return text.split('\n')
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
 * @param source The file text the editor was loaded with.
 * @param loaded The editor's serialization right after loading `source`.
 * @param edited The editor's serialization now.
 * @returns `source` with the user's changes: stretches the edit did not touch keep their original lines.
 */
export function preserveSource(source: string, loaded: string, edited: string): string {
  if (edited === loaded) return source
  const before = lines(loaded)
  const original = lines(source)
  // For each line of `loaded`: kept by the edit or not, and the edited lines inserted in front of it.
  const kept = new Array<boolean>(before.length).fill(true)
  const inserted: string[][] = Array.from({ length: before.length + 1 }, () => [])
  let at = 0
  for (const part of diffArrays(before, lines(edited))) {
    const count = part.count ?? part.value.length
    if (part.added) inserted[at]!.push(...part.value)
    else if (part.removed) { for (let index = 0; index < count; index++) kept[at + index] = false; at += count } else at += count
  }
  const out: string[] = []
  const placed = new Set<number>()
  const insertAt = (index: number): void => { if (!placed.has(index)) { placed.add(index); out.push(...inserted[index]!) } }
  for (const block of blocks(before, original)) {
    insertAt(block.loadedStart)
    let touched = false
    for (let index = block.loadedStart; index < block.loadedEnd; index++) {
      if (!kept[index] || (index > block.loadedStart && inserted[index]!.length > 0)) { touched = true; break }
    }
    if (!touched) { out.push(...original.slice(block.sourceStart, block.sourceEnd)); continue }
    for (let index = block.loadedStart; index < block.loadedEnd; index++) {
      if (index > block.loadedStart) insertAt(index)
      if (kept[index]) out.push(before[index]!)
    }
  }
  insertAt(before.length)
  return out.join('\n')
}
