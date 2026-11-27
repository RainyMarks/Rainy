/** Small shared utilities for viewer pages: Base64 encoding, little/big-endian reads, HTML/SVG escaping and errors. */

/** East Asian wide characters, which take about two columns in fixed-width estimates. */
export const CJK_WIDE = /[\u1100-\u115f\u2e80-\ua4cf\uac00-\ud7a3\uf900-\ufaff\ufe30-\ufe4f\uff00-\uff60\uffe0-\uffe6]/u

/**
 * Encode arbitrary bytes as Base64 in 32 KiB chunks so large buffers never overflow the JS argument stack.
 * @param bytes Binary payload.
 * @returns Standard Base64 string.
 */
export function encodeBase64(bytes: Uint8Array): string {
  let binary = ''
  for (let offset = 0; offset < bytes.length; offset += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000))
  }
  return btoa(binary)
}

/**
 * Escape the five HTML/SVG special characters as numeric character references.
 * @param text Raw text.
 * @returns Escaped text safe for HTML and SVG text content or attributes.
 */
export function escapeHtml(text: string): string {
  return text.replace(/[&<>"']/gu, character => `&#${character.charCodeAt(0)};`)
}

/** @param error Thrown value. @returns Human-readable error message. */
export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/** Unsigned 16-bit little-endian integer at `offset`, zero-padded past end of buffer. */
export const readU16Le = (bytes: Uint8Array, offset: number): number => (bytes[offset] ?? 0) | ((bytes[offset + 1] ?? 0) << 8)
/** Unsigned 16-bit big-endian integer at `offset`, zero-padded past end of buffer. */
export const readU16Be = (bytes: Uint8Array, offset: number): number => ((bytes[offset] ?? 0) << 8) | (bytes[offset + 1] ?? 0)
/** Unsigned 32-bit little-endian integer at `offset`, zero-padded past end of buffer. */
export const readU32Le = (bytes: Uint8Array, offset: number): number => readU16Le(bytes, offset) + readU16Le(bytes, offset + 2) * 0x10000
/** Unsigned 32-bit big-endian integer at `offset`, zero-padded past end of buffer. */
export const readU32Be = (bytes: Uint8Array, offset: number): number => readU16Be(bytes, offset) * 0x10000 + readU16Be(bytes, offset + 2)
