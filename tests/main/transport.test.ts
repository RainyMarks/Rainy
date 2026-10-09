import { describe, it, expect } from 'vitest'
import { parseReady } from '../src/transport.ts'
import { parseModelSetup, DEEPSEEK_FLASH } from '../src/models.ts'
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
describe('model setup validation', () => {
  it('uses actual official model id and maximum configuration', () => {
    expect(parseModelSetup(DEEPSEEK_FLASH)).toEqual(DEEPSEEK_FLASH)
  })
  it('keeps Chat Completions as the local default without requiring a secret', () => {
    const model = parseModelSetup({ provider: 'local', baseURL: 'http://127.0.0.1:1234/v1', model: 'local-model', contextWindow: 100000, local: true })
    expect(model.api).toBe('openai-completions')
    expect(model.apiKey).toBeUndefined()
  })
  it('defaults API models to Responses while honoring an explicit protocol', () => {
    const input = { ...DEEPSEEK_FLASH, api: undefined }
    expect(parseModelSetup(input).api).toBe('openai-responses')
    expect(parseModelSetup({ ...input, api: 'anthropic-messages' }).api).toBe('anthropic-messages')
  })
  it('does not echo invalid keys in diagnostics', () => {
    expect(() => parseModelSetup({ ...DEEPSEEK_FLASH, baseURL: 'http://key:secret@example.com' })).toThrow('服务地址必须')
  })
})
