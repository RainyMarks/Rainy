/** Compact byte counts for download sizes and attachments. */

/**
 * Byte count as compact size text (`312B`, `4.2KB`, `1.5MB`, `2.4GB`).
 * @param bytes Exact byte count.
 * @returns Whole-unit text with one decimal below ten of the chosen unit.
 */
export function fileSizeText(bytes: number): string {
  if (bytes < 1024) return `${bytes}B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb < 10 ? kb.toFixed(1) : Math.round(kb)}KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)}MB`
  const gb = mb / 1024
  return `${gb < 10 ? gb.toFixed(1) : Math.round(gb)}GB`
}
