import type { IncomingMessage } from 'node:http'
import { Readable } from 'node:stream'
import { describe, expect, it } from 'vitest'
import { readRequestBytes } from '../src/request-body.ts'

function request(chunks: readonly Buffer[], headers: IncomingMessage['headers'] = {}): IncomingMessage {
  return Object.assign(Readable.from(chunks), { headers }) as never
}

describe('bounded request bodies', () => {
  it('keeps a multi-byte character split across chunks intact', async () => {
    const bytes = Buffer.from(JSON.stringify({ text: '项目记忆' }))
    const split = bytes.indexOf(Buffer.from('记')) + 1
    const body = await readRequestBytes(request([bytes.subarray(0, split), bytes.subarray(split)]), 1024, () => new Error('too large'))
    expect(JSON.parse(body.toString('utf8'))).toEqual({ text: '项目记忆' })
  })

  it('rejects a body over its budget, declared or streamed', async () => {
    const tooLarge = () => new Error('too large')
    await expect(readRequestBytes(request([Buffer.alloc(8)], { 'content-length': '9' }), 8, tooLarge)).rejects.toThrow('too large')
    await expect(readRequestBytes(request([Buffer.alloc(5), Buffer.alloc(5)]), 8, tooLarge)).rejects.toThrow('too large')
    await expect(readRequestBytes(request([Buffer.alloc(8)]), 8, tooLarge)).resolves.toHaveLength(8)
  })
})
