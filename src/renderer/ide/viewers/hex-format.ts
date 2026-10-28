/** Text forms of bytes for the hex view: rows, copy formats, offsets and the data inspector. */

/** Bytes shown on one row. */
export const HEX_ROW_BYTES = 16

const HEX = Array.from({ length: 256 }, (_unused, value) => value.toString(16).padStart(2, '0'))

/** @param byte One byte. @returns Two lowercase hex digits. */
export function hexByte(byte: number): string {
  return HEX[byte] ?? '??'
}

/** @param byte One byte. @returns The printable ASCII character, or `.`. */
export function asciiByte(byte: number): string {
  return byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : '.'
}

/**
 * @param offset File offset.
 * @param size File size, which decides the digit count.
 * @returns Uppercase hex offset padded to at least 8 digits.
 */
export function formatOffset(offset: number, size: number): string {
  const digits = Math.max(8, Math.max(1, size - 1).toString(16).length)
  return offset.toString(16).toUpperCase().padStart(digits, '0')
}

/**
 * Parse a user-typed offset: `0x1F`, `1Fh` or `$1F` is hex, plain digits are decimal.
 * @param text Input.
 * @returns The offset, or `undefined` when it is not a number.
 */
export function parseOffset(text: string): number | undefined {
  const value = text.trim().replaceAll('_', '')
  const hex = /^(?:0x|\$)([0-9a-f]+)$/iu.exec(value) ?? /^([0-9a-f]+)h$/iu.exec(value)
  const parsed = hex !== null ? Number.parseInt(hex[1]!, 16) : /^\d+$/u.test(value) ? Number(value) : Number.NaN
  return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : undefined
}

/** Clipboard forms of a byte selection. */
export type CopyFormat = 'hex' | 'text' | 'python' | 'c' | 'base64'

/**
 * @param bytes Selected bytes.
 * @param format Target notation.
 * @returns The selection in that notation.
 */
export function formatBytes(bytes: Uint8Array, format: CopyFormat): string {
  switch (format) {
    case 'hex': return Array.from(bytes, hexByte).join(' ')
    case 'text': return Array.from(bytes, asciiByte).join('')
    case 'python': return `b'${Array.from(bytes, byte => byte >= 0x20 && byte <= 0x7e && byte !== 0x27 && byte !== 0x5c ? String.fromCharCode(byte) : `\\x${hexByte(byte)}`).join('')}'`
    case 'c': return `{ ${Array.from(bytes, byte => `0x${hexByte(byte)}`).join(', ')} }`
    case 'base64': {
      let binary = ''
      for (const byte of bytes) binary += String.fromCharCode(byte)
      return btoa(binary)
    }
  }
}

/** One interpretation of the bytes at the cursor. */
export interface InspectorRow {
  readonly label: string
  readonly value: string
}

/**
 * Interpret the bytes at the cursor as the common scalar types.
 * @param bytes Up to 8 bytes starting at the cursor; fewer near the end of the file.
 * @param littleEndian Byte order of multi-byte values.
 * @returns Rows for every type the available bytes can hold.
 */
export function inspectBytes(bytes: Uint8Array, littleEndian: boolean): InspectorRow[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  const rows: InspectorRow[] = []
  if (bytes.length >= 1) {
    rows.push({ label: 'binary', value: bytes[0]!.toString(2).padStart(8, '0') })
    rows.push({ label: 'uint8', value: String(view.getUint8(0)) }, { label: 'int8', value: String(view.getInt8(0)) })
  }
  if (bytes.length >= 2) rows.push({ label: 'uint16', value: String(view.getUint16(0, littleEndian)) }, { label: 'int16', value: String(view.getInt16(0, littleEndian)) })
  if (bytes.length >= 4) {
    rows.push({ label: 'uint32', value: String(view.getUint32(0, littleEndian)) }, { label: 'int32', value: String(view.getInt32(0, littleEndian)) })
    rows.push({ label: 'float32', value: String(view.getFloat32(0, littleEndian)) })
  }
  if (bytes.length >= 8) {
    rows.push({ label: 'uint64', value: view.getBigUint64(0, littleEndian).toString() }, { label: 'int64', value: view.getBigInt64(0, littleEndian).toString() })
    rows.push({ label: 'float64', value: String(view.getFloat64(0, littleEndian)) })
  }
  if (bytes.length >= 1) rows.push({ label: 'UTF-8', value: firstCharacter(bytes) })
  return rows
}

function firstCharacter(bytes: Uint8Array): string {
  const lead = bytes[0]!
  const length = lead < 0x80 ? 1 : lead >= 0xf0 ? 4 : lead >= 0xe0 ? 3 : lead >= 0xc0 ? 2 : 0
  if (length === 0 || bytes.length < length) return '—'
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(bytes.subarray(0, length))
    const code = text.codePointAt(0) ?? 0
    return code < 0x20 || code === 0x7f ? `U+${code.toString(16).toUpperCase().padStart(4, '0')}` : `${text} (U+${code.toString(16).toUpperCase().padStart(4, '0')})`
  } catch (_invalidSequence) {
    return '—'
  }
}

/** Encodings the hex view reads a file as text in, as 3.x offered them; the page decodes with `TextDecoder`. */
export const TEXT_ENCODINGS = ['utf-8', 'gb18030', 'big5', 'shift_jis', 'euc-kr', 'utf-16le', 'utf-16be', 'windows-1252'] as const
export type TextEncoding = (typeof TEXT_ENCODINGS)[number]

/**
 * The encoding a file most likely uses: a byte order mark says so; else UTF-8 when its start is valid UTF-8 (a sequence
 * cut at the end does not count), else GB18030, the encoding of Chinese Windows.
 * @param head The first bytes of the file.
 * @returns One of {@link TEXT_ENCODINGS}.
 */
export function guessEncoding(head: Uint8Array): TextEncoding {
  if (head[0] === 0xff && head[1] === 0xfe) return 'utf-16le'
  if (head[0] === 0xfe && head[1] === 0xff) return 'utf-16be'
  try {
    new TextDecoder('utf-8', { fatal: true }).decode(head, { stream: true })
    return 'utf-8'
  } catch (_invalidSequence) {
    return 'gb18030'
  }
}

/**
 * @param bytes The start of a file, or all of it.
 * @param encoding One of {@link TEXT_ENCODINGS}.
 * @param complete Whether `bytes` ends where the file ends; otherwise a character cut at the end is left out.
 * @returns The text; bytes the encoding does not map show as U+FFFD, and a byte order mark is dropped.
 */
export function decodeText(bytes: Uint8Array, encoding: TextEncoding, complete: boolean): string {
  return new TextDecoder(encoding).decode(bytes, { stream: !complete })
}
