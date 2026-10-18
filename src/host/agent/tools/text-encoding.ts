/**
 * Encodings the file tools read and keep: UTF-8, UTF-16 with a byte-order mark (what Windows PowerShell 5.1 redirects
 * to), and GB18030, the legacy code page of Chinese Windows. Text that is not UTF-8 is written back in its own encoding
 * only when its bytes convert back unchanged, so a change never damages the rest of the file.
 */

/** A text encoding the file tools keep. */
export type FileEncoding = 'utf-8' | 'utf-16le' | 'utf-16be' | 'gb18030'

/** A file's text and how it was stored. */
export interface DecodedFile {
  /** Text without the byte-order mark. */
  text: string
  encoding: FileEncoding
  bom: boolean
  /** UTF-8 with a few invalid bytes, shown replaced; writing it back would change them. */
  damaged?: true | undefined
}

/** Bytes searched for NUL to tell a binary file from text that is not UTF-16. */
const SNIFF_BYTES = 8192

/**
 * Decode a file's bytes.
 * @param bytes File content.
 * @returns The text, or `undefined` for binary content and bytes that are neither UTF-8, UTF-16 with a mark nor GB18030.
 */
export function decodeFile(bytes: Uint8Array): DecodedFile | undefined {
  if (bytes[0] === 0xff && bytes[1] === 0xfe) return { text: new TextDecoder('utf-16le', { ignoreBOM: true }).decode(bytes.subarray(2)), encoding: 'utf-16le', bom: true }
  if (bytes[0] === 0xfe && bytes[1] === 0xff) return { text: new TextDecoder('utf-16be', { ignoreBOM: true }).decode(bytes.subarray(2)), encoding: 'utf-16be', bom: true }
  if (bytes.subarray(0, SNIFF_BYTES).includes(0)) return undefined
  const bom = bytes[0] === 0xef && bytes[1] === 0xbb && bytes[2] === 0xbf
  const body = bom ? bytes.subarray(3) : bytes
  try {
    return { text: new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(body), encoding: 'utf-8', bom }
  } catch (_notUtf8) {
    // A UTF-8 file with a stray invalid byte is not GB18030: reading it as such would store new text in the wrong encoding.
    if (bom || mostlyUtf8(body)) return { text: new TextDecoder('utf-8', { ignoreBOM: true }).decode(body), encoding: 'utf-8', bom, damaged: true }
  }
  try {
    return { text: new TextDecoder('gb18030', { fatal: true }).decode(bytes), encoding: 'gb18030', bom: false }
  } catch (_notGb18030) {
    return undefined
  }
}

/**
 * @param bytes Text that is not valid UTF-8.
 * @returns Whether nearly all of its non-ASCII bytes still form UTF-8 sequences, as in a UTF-8 file with a damaged byte.
 *   GBK text does not: only about one in six of its two-byte codes happens to form a UTF-8 sequence.
 */
function mostlyUtf8(bytes: Uint8Array): boolean {
  let valid = 0
  let invalid = 0
  for (let index = 0; index < bytes.length;) {
    const lead = bytes[index]!
    if (lead < 0x80) { index++; continue }
    const length = lead >= 0xc2 && lead <= 0xdf ? 2 : lead >= 0xe0 && lead <= 0xef ? 3 : lead >= 0xf0 && lead <= 0xf4 ? 4 : 0
    let ok = length > 0 && index + length <= bytes.length
    for (let offset = 1; ok && offset < length; offset++) ok = (bytes[index + offset]! & 0xc0) === 0x80
    if (ok) { valid++; index += length } else { invalid++; index++ }
  }
  return invalid * 4 < valid
}

/**
 * Encode text the way a file was stored.
 * @param file Encoding and byte-order mark of the file.
 * @param text Text without a byte-order mark.
 * @returns The bytes, or `undefined` when the encoding cannot store some character of the text.
 */
export function encodeFile(file: Pick<DecodedFile, 'encoding' | 'bom'>, text: string): Uint8Array | undefined {
  switch (file.encoding) {
    case 'utf-8': return Buffer.from(file.bom ? `\ufeff${text}` : text, 'utf8')
    case 'utf-16le': return Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, 'utf16le')])
    case 'utf-16be': return Buffer.concat([Buffer.from([0xfe, 0xff]), Buffer.from(text, 'utf16le').swap16()])
    case 'gb18030': return encodeGb18030(text)
  }
}

/**
 * Whether a decoded file converts back to exactly its bytes; undamaged UTF-8 always does.
 * @param bytes File content.
 * @param file Its decoded form.
 * @returns Whether the file can be written back in its own encoding without changing untouched text.
 */
export function roundTrips(bytes: Uint8Array, file: DecodedFile): boolean {
  if (file.encoding === 'utf-8') return file.damaged !== true
  const encoded = encodeFile(file, file.text)
  return encoded !== undefined && Buffer.from(encoded).equals(bytes)
}

/** @param encoding A file encoding. @returns Its name as the model and the user know it. */
export function encodingLabel(encoding: FileEncoding): string {
  return encoding === 'gb18030' ? 'GB18030 (GBK)' : encoding.toUpperCase()
}

let gbTable: Map<number, number> | undefined

/** Two-byte GB18030 codes by code point, built once from the platform decoder; the first code wins for duplicates. */
function gb18030Table(): Map<number, number> {
  if (gbTable !== undefined) return gbTable
  const table = new Map<number, number>()
  const decoder = new TextDecoder('gb18030')
  const pair = new Uint8Array(2)
  for (let lead = 0x81; lead <= 0xfe; lead++) {
    for (let trail = 0x40; trail <= 0xfe; trail++) {
      if (trail === 0x7f) continue
      pair[0] = lead
      pair[1] = trail
      const text = decoder.decode(pair)
      const point = text.codePointAt(0)
      if (text.length === 1 && point !== undefined && point !== 0xfffd && !table.has(point)) table.set(point, (lead << 8) | trail)
    }
  }
  gbTable = table
  return table
}

/**
 * Encode text as GB18030 with its one- and two-byte codes, which hold ASCII and the GBK repertoire.
 * @param text Text to encode.
 * @returns The bytes, or `undefined` when a character needs a four-byte code.
 */
function encodeGb18030(text: string): Uint8Array | undefined {
  const table = gb18030Table()
  const out = Buffer.allocUnsafe(text.length * 2)
  let length = 0
  for (const char of text) {
    const point = char.codePointAt(0)!
    if (point < 0x80) { out[length++] = point; continue }
    const code = table.get(point)
    if (code === undefined) return undefined
    out[length++] = code >> 8
    out[length++] = code & 0xff
  }
  return out.subarray(0, length)
}
