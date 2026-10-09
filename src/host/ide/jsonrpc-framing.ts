/** `Content-Length` framing of JSON-RPC messages, shared by language servers (LSP) and debug adapters (DAP). */

const HEADER_SEPARATOR = '\r\n\r\n'
/** A peer that never sends the separator cannot grow the header buffer past this size. */
const MAX_HEADER_BYTES = 1 << 16

/**
 * Frame one message as `Content-Length: N\r\n\r\n<UTF-8 JSON>`.
 * @param message JSON-serializable message.
 * @returns The framed bytes.
 */
export function encodeMessage(message: unknown): Buffer {
  const body = Buffer.from(JSON.stringify(message), 'utf8')
  return Buffer.concat([Buffer.from(`Content-Length: ${body.length}\r\n\r\n`, 'ascii'), body])
}

function contentLength(header: string): number {
  for (const line of header.split('\r\n')) {
    const colon = line.indexOf(':')
    if (colon < 0 || line.slice(0, colon).trim().toLowerCase() !== 'content-length') continue
    const value = Number(line.slice(colon + 1).trim())
    if (!Number.isInteger(value) || value < 0) throw new Error(`invalid Content-Length header: ${JSON.stringify(line)}`)
    return value
  }
  throw new Error(`Message header block missing Content-Length: ${JSON.stringify(header)}`)
}

/** Streaming decoder that yields each complete message body; headers other than `Content-Length` are ignored. */
export class MessageDecoder {
  private buffer: Buffer = Buffer.alloc(0)

  /** @param maxMessageBytes Largest accepted body; a larger declared length fails the stream. */
  constructor(private readonly maxMessageBytes: number) {}

  /**
   * Append received bytes.
   * @param chunk Bytes read from the peer.
   * @returns Every message completed by this chunk, in arrival order.
   * @throws Error when a header is malformed, a body exceeds the limit or a body is not JSON.
   */
  push(chunk: Buffer): unknown[] {
    this.buffer = this.buffer.length === 0 ? chunk : Buffer.concat([this.buffer, chunk])
    const messages: unknown[] = []
    for (;;) {
      const separator = this.buffer.indexOf(HEADER_SEPARATOR)
      if (separator < 0) {
        if (this.buffer.length > MAX_HEADER_BYTES) throw new Error(`Message header exceeded ${MAX_HEADER_BYTES} bytes without a terminator`)
        return messages
      }
      if (separator > MAX_HEADER_BYTES) throw new Error(`Message header exceeded ${MAX_HEADER_BYTES} bytes`)
      const length = contentLength(this.buffer.toString('ascii', 0, separator))
      if (length > this.maxMessageBytes) throw new Error(`Message length ${length} exceeds the ${this.maxMessageBytes}-byte limit`)
      const start = separator + HEADER_SEPARATOR.length
      if (this.buffer.length < start + length) return messages
      const body = this.buffer.toString('utf8', start, start + length)
      this.buffer = this.buffer.subarray(start + length)
      try {
        messages.push(JSON.parse(body))
      } catch (error) {
        throw new Error(`Message body was not valid JSON: ${error instanceof Error ? error.message : String(error)}`)
      }
    }
  }
}
