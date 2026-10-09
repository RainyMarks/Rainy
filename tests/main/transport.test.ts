import { describe, it, expect } from 'vitest'
import { parseReady } from '../../src/main/transport.ts'
describe('WSL control protocol', () => {
  it('accepts a matching loopback host', () => {
    expect(parseReady({ protocol: 1, url: 'http://127.0.0.1:23456/?token=x', pid: 1, home: '/home/a/.rainy-agent' }).pid).toBe(1)
  })
  it.each(['https://example.com/', 'file:///etc/passwd', 'http://localhost:1234/', 'http://user:password@127.0.0.1:1234/'])('rejects unexpected Host URL %s', (url) => {
    expect(() => parseReady({ protocol: 1, url, pid: 1, home: '/tmp' })).toThrow()
  })
  it('refuses future protocols', () => {
    expect(() => parseReady({ protocol: 2, url: 'http://127.0.0.1:1234', pid: 1, home: '/tmp' })).toThrow()
  })
  it('rejects process group identifiers', () => {
    expect(() => parseReady({ protocol: 1, url: 'http://127.0.0.1:1234', pid: -1, home: '/tmp' })).toThrow()
  })
})
