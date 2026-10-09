/** DAP transport request, framing, cancellation and reverse-request lifecycle evidence. */
import { PassThrough } from 'node:stream'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { encodeMessage, MessageDecoder } from '@deepseek-ai/dsh-lsp-stdio'
import { IdeDapPeer } from '../src/ide-debug-protocol.ts'

const cleanups: (() => Promise<void>)[] = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
  vi.useRealTimers()
})

function fixture(onRequest: (command: string, body: unknown) => Promise<unknown> = async () => ({})) {
  const input = new PassThrough()
  const output = new PassThrough()
  const controller = new AbortController()
  const messages: unknown[] = []
  const decoder = new MessageDecoder(4096)
  output.on('data', (chunk: Buffer) => {
    messages.push(...decoder.push(chunk))
  })
  const onFailure = vi.fn()
  const onEvent = vi.fn()
  const peer = new IdeDapPeer({
    input,
    output,
    signal: controller.signal,
    maxMessageBytes: 4096,
    requestTimeoutMs: 100,
    onEvent,
    onRequest,
    onFailure,
    reportCallbackError: vi.fn(),
  })
  cleanups.push(async () => {
    await peer.close()
    input.destroy()
    output.destroy()
  })
  return { peer, input, output, controller, messages, onFailure, onEvent }
}

describe('owned DAP streams', () => {
  it('correlates out-of-order responses and accepts frames split across byte chunks', async () => {
    const f = fixture()
    const first = f.peer.request('threads', {})
    const second = f.peer.request('stackTrace', { threadId: 7 })
    const frame = encodeMessage({
      seq: 4,
      type: 'response',
      request_seq: 2,
      command: 'stackTrace',
      success: true,
      body: { stackFrames: [] },
    })
    f.input.write(frame.subarray(0, 8))
    f.input.write(frame.subarray(8))
    f.input.write(
      encodeMessage({
        seq: 5,
        type: 'response',
        request_seq: 1,
        command: 'threads',
        success: true,
        body: { threads: [{ id: 7, name: 'main' }] },
      }),
    )
    await expect(second).resolves.toEqual({ stackFrames: [] })
    await expect(first).resolves.toEqual({ threads: [{ id: 7, name: 'main' }] })
    expect(f.onFailure).not.toHaveBeenCalled()
  })

  it('rejects a timed-out request without misassigning its late response', async () => {
    vi.useFakeTimers()
    const f = fixture()
    const request = f.peer.request('threads', {})
    const rejected = expect(request).rejects.toThrow('deadline')
    await vi.advanceTimersByTimeAsync(101)
    await rejected
    f.input.write(encodeMessage({ type: 'response', request_seq: 1, command: 'threads', success: true, body: { threads: [] } }))
    expect(f.onFailure).not.toHaveBeenCalled()
  })

  it('waits for the owned target initialization after an adapter bootstrap event', async () => {
    const f = fixture()
    f.input.write(encodeMessage({ type: 'event', event: 'initialized' }))
    await f.peer.initialized
    let initialized = false
    const target = f.peer.initializedAfter(f.peer.initializationSequence).then(() => {
      initialized = true
    })
    await Promise.resolve()
    expect(initialized).toBe(false)
    f.input.write(encodeMessage({ type: 'event', event: 'initialized' }))
    await target
    expect(f.peer.initializationSequence).toBe(2)
  })

  it('waits for admitted reverse handlers and suppresses late responses after close', async () => {
    let finish!: (value: unknown) => void
    const gate = new Promise<unknown>((resolve) => {
      finish = resolve
    })
    const f = fixture(async () => gate)
    f.input.write(encodeMessage({ type: 'request', seq: 10, command: 'runInTerminal', arguments: {} }))
    let closed = false
    const close = f.peer.close().then(() => {
      closed = true
    })
    await Promise.resolve()
    expect(closed).toBe(false)
    finish({ processId: 5 })
    await close
    expect(f.messages).toEqual([])
  })

  it('rejects pending requests on cancellation and contains late transport errors', async () => {
    const f = fixture()
    const request = f.peer.request('initialize', {})
    const rejected = expect(request).rejects.toThrow('stopped')
    f.controller.abort()
    await rejected
    expect(() => {
      f.output.emit('error', new Error('late EPIPE'))
    }).not.toThrow()
    expect(f.onFailure).not.toHaveBeenCalled()
  })

  it('fails oversized incoming frames before processing events', async () => {
    const f = fixture()
    f.input.write(Buffer.from('Content-Length: 100000\r\n\r\n'))
    expect(f.onFailure).toHaveBeenCalledTimes(1)
    expect(f.onEvent).not.toHaveBeenCalled()
  })
})
