// @vitest-environment happy-dom
/** A request lost with its connection fails with the message the page chose for the interface language. */
import { afterEach, expect, it, vi } from 'vitest'

afterEach(() => { vi.unstubAllGlobals(); vi.resetModules() })

it('rejects requests sent on a lost connection with the described message', async () => {
  const sockets: FakeSocket[] = []
  class FakeSocket extends EventTarget {
    static readonly OPEN = 1
    readyState = 0
    constructor(readonly url: string | URL) { super(); sockets.push(this) }
    send(): void {}
    close(): void {}
  }
  vi.stubGlobal('WebSocket', FakeSocket)
  const { describeDisconnection, host } = await import('../../src/renderer/rpc.ts')
  describeDisconnection(() => '与 RainyAgent 的连接已断开，这次操作没有完成。')
  const socket = sockets[0]!
  socket.readyState = FakeSocket.OPEN
  socket.dispatchEvent(new Event('open'))
  const request = host.call('prefs.get')
  socket.dispatchEvent(new Event('close'))
  await expect(request).rejects.toMatchObject({ code: 'disconnected', message: '与 RainyAgent 的连接已断开，这次操作没有完成。' })
})
