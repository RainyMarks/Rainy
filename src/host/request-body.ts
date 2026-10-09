/** Bounded request bodies shared by the Host's authenticated JSON routes. */
import type { IncomingMessage } from 'node:http'

/**
 * Collect a complete request body under a byte budget before any decoding,
 * so a multi-byte character split across network chunks stays intact.
 * @param request - authenticated incoming request.
 * @param maxBytes - maximum complete body size.
 * @param tooLarge - creates the route's failure once the budget is exceeded.
 * @returns the undecoded body bytes.
 */
export async function readRequestBytes(request: IncomingMessage, maxBytes: number, tooLarge: () => Error): Promise<Buffer> {
  const declared = request.headers['content-length']
  if (declared !== undefined && (!/^\d+$/u.test(declared) || Number(declared) > maxBytes)) throw tooLarge()
  const chunks: Buffer[] = []
  let bytes = 0
  for await (const chunk of request) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk))
    bytes += buffer.length
    if (bytes > maxBytes) throw tooLarge()
    chunks.push(buffer)
  }
  return Buffer.concat(chunks)
}
