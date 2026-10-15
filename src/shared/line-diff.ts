/** Line counts of a change, for the +/- figures next to a changed file. */

/** Above this many differing lines the texts count as replaced rather than compared exactly. */
const MAX_EDITS = 4000

/** @returns The lines of a text; a final line break ends the last line rather than starting an empty one. */
function lines(text: string): string[] {
  const normalized = text.replaceAll('\r\n', '\n')
  if (normalized.length === 0) return []
  return (normalized.endsWith('\n') ? normalized.slice(0, -1) : normalized).split('\n')
}

/**
 * Count the lines a change added and removed, with Myers' diff on lines.
 * @param before Text before the change.
 * @param after Text after it.
 * @returns Added and removed lines; very different texts count as wholly replaced.
 */
export function lineChangeCounts(before: string, after: string): { added: number; removed: number } {
  const a = lines(before)
  const b = lines(after)
  let head = 0
  while (head < a.length && head < b.length && a[head] === b[head]) head++
  let tail = 0
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++
  const n = a.length - head - tail
  const m = b.length - head - tail
  if (n === 0 || m === 0) return { added: m, removed: n }
  const max = Math.min(n + m, MAX_EDITS)
  const offset = max + 1
  const v = new Int32Array(2 * max + 3)
  for (let d = 0; d <= max; d++) {
    for (let k = -d; k <= d; k += 2) {
      let x = k === -d || (k !== d && v[offset + k - 1]! < v[offset + k + 1]!) ? v[offset + k + 1]! : v[offset + k - 1]! + 1
      let y = x - k
      while (x < n && y < m && a[head + x] === b[head + y]) { x++; y++ }
      v[offset + k] = x
      // `d` edits remain: n - removed lines are common, and so are m - added.
      if (x >= n && y >= m) return { added: (d - n + m) / 2, removed: (d + n - m) / 2 }
    }
  }
  return { added: m, removed: n }
}
