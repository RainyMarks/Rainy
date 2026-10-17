/** What small Windows bookkeeping files say: the owner in an Office `~$` lock file and the target of a `.lnk` shortcut. */

const u16 = (bytes: Uint8Array, offset: number): number => (bytes[offset] ?? 0) | ((bytes[offset + 1] ?? 0) << 8)
const u32 = (bytes: Uint8Array, offset: number): number => u16(bytes, offset) + u16(bytes, offset + 2) * 0x10000

function utf16(bytes: Uint8Array, offset: number, units: number): string {
  return new TextDecoder('utf-16le').decode(bytes.subarray(offset, Math.min(bytes.length, offset + units * 2)))
}

/** A NUL-terminated string at `offset`, in UTF-16 or in the system code page (GB18030 covers Chinese Windows). */
function terminated(bytes: Uint8Array, offset: number, unicode: boolean): string {
  if (offset <= 0 || offset >= bytes.length) return ''
  let end = offset
  if (unicode) {
    while (end + 1 < bytes.length && (bytes[end] !== 0 || bytes[end + 1] !== 0)) end += 2
    return utf16(bytes, offset, (end - offset) / 2)
  }
  while (end < bytes.length && bytes[end] !== 0) end++
  return new TextDecoder('gb18030').decode(bytes.subarray(offset, end))
}

/**
 * The user who has the document open, from an Office owner file: a length byte and the name in the system code page,
 * then at 0x36 a 16-bit length and the name in UTF-16.
 * @param bytes The whole file; owner files are 162 or 165 bytes.
 * @returns The name, or `undefined` when the file does not look like an owner file.
 */
export function officeLockOwner(bytes: Uint8Array): string | undefined {
  const wide = u16(bytes, 0x36)
  if (wide > 0 && wide <= 54 && 0x38 + wide * 2 <= bytes.length) {
    const name = utf16(bytes, 0x38, wide).trim()
    if (name !== '' && !name.includes('\0')) return name
  }
  const length = bytes[0] ?? 0
  if (length === 0 || length > 53 || 1 + length > bytes.length) return undefined
  const name = new TextDecoder('gb18030').decode(bytes.subarray(1, 1 + length)).trim()
  return name === '' || name.includes('\0') ? undefined : name
}

/** What a shortcut points at, from the Shell Link format ([MS-SHLLINK]). */
export interface ShortcutFacts {
  /** Full path of the target on the computer that made the shortcut. */
  readonly target?: string | undefined
  readonly arguments?: string | undefined
  readonly workingDirectory?: string | undefined
  readonly description?: string | undefined
}

const LINK_CLSID = [0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]

/**
 * @param bytes The whole .lnk file.
 * @returns The shortcut's target and string data, or `undefined` when the file is not a shell link.
 */
export function shortcutFacts(bytes: Uint8Array): ShortcutFacts | undefined {
  if (bytes.length < 0x4c || u32(bytes, 0) !== 0x4c || !LINK_CLSID.every((value, index) => bytes[4 + index] === value)) return undefined
  const flags = u32(bytes, 0x14)
  const unicode = (flags & 0x80) !== 0
  let offset = 0x4c
  if ((flags & 0x01) !== 0) offset += 2 + u16(bytes, offset)
  let target: string | undefined
  if ((flags & 0x02) !== 0 && offset + 0x1c <= bytes.length) {
    const info = offset
    const size = u32(bytes, info)
    const header = u32(bytes, info + 4)
    const infoFlags = u32(bytes, info + 8)
    if ((infoFlags & 0x01) !== 0) {
      const wide = header >= 0x24 ? u32(bytes, info + 0x1c) : 0
      const base = wide !== 0 ? terminated(bytes, info + wide, true) : terminated(bytes, info + u32(bytes, info + 0x10), false)
      const suffixWide = header >= 0x24 ? u32(bytes, info + 0x20) : 0
      const suffix = suffixWide !== 0 ? terminated(bytes, info + suffixWide, true) : terminated(bytes, info + u32(bytes, info + 0x18), false)
      target = suffix === '' ? base : base.endsWith('\\') ? base + suffix : `${base}\\${suffix}`
    }
    offset = info + size
  }
  const read = (present: boolean): string | undefined => {
    if (!present || offset + 2 > bytes.length) return undefined
    const count = u16(bytes, offset)
    offset += 2
    const length = unicode ? count * 2 : count
    const text = unicode ? utf16(bytes, offset, count) : new TextDecoder('gb18030').decode(bytes.subarray(offset, offset + length))
    offset += length
    return text === '' ? undefined : text
  }
  const description = read((flags & 0x04) !== 0)
  const relative = read((flags & 0x08) !== 0)
  const workingDirectory = read((flags & 0x10) !== 0)
  const args = read((flags & 0x20) !== 0)
  return { target: target ?? relative, arguments: args, workingDirectory, description }
}
