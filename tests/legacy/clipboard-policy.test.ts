import { describe, expect, it } from 'vitest'
import { allowsClipboardWrite } from '../src/clipboard-policy.ts'

describe('desktop clipboard permissions', () => {
  it('allows conversation and workbench writes on the authenticated Host', () => {
    for (const path of ['/', '/sessions/example', '/rainy/icesky/index.html']) {
      expect(allowsClipboardWrite('clipboard-sanitized-write', `http://127.0.0.1:1234${path}`, 'http://127.0.0.1:1234')).toBe(true)
    }
  })
  it('rejects clipboard reads, foreign origins, malformed URLs and missing origins', () => {
    expect(allowsClipboardWrite('clipboard-read', 'http://127.0.0.1:1234/', 'http://127.0.0.1:1234')).toBe(false)
    for (const url of [undefined, 'invalid', 'file:///index.html', 'http://127.0.0.1:12345/', 'https://example.com/']) {
      expect(allowsClipboardWrite('clipboard-sanitized-write', url, 'http://127.0.0.1:1234')).toBe(false)
    }
  })
})
