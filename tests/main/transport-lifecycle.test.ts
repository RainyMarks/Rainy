/** Controlled process and socket barriers for the Windows-to-WSL readiness handoff. */
import { ChildProcess } from 'node:child_process'
import type { ChildProcessWithoutNullStreams } from 'node:child_process'
import { PassThrough } from 'node:stream'
import { createConnection, Socket } from 'node:net'
import { describe, expect, it, vi } from 'vitest'
import { WindowsHostTransport, WslHostTransport } from '../../src/main/transport.ts'
import type { HostOptions } from '../../src/main/transport.ts'

vi.mock('node:net', async original => ({ ...await original<typeof import('node:net')>(), createConnection: vi.fn() }))

function fixture(
  onTestFinished: (cleanup: () => void | Promise<void>) => void, native = false, cooperative = true,
  callbacks: Pick<HostOptions, 'onDiagnostic' | 'onExit' | 'resolveProxy' | 'credentialKey'> = {},
) {
  vi.useFakeTimers()
  const input = new PassThrough()
  const output = new PassThrough()
  const errors = new PassThrough()
  const child: ChildProcessWithoutNullStreams = Object.assign(new ChildProcess(), {
    stdin: input, stdout: output, stderr: errors, stdio: [input, output, errors, undefined, undefined] as const,
  })
  let closed = false
  let forced = 0
  let launched = 0
  const close = (code = 0) => {
    if (closed) return
    closed = true
    Object.defineProperty(child, 'exitCode', { value: code })
    child.emit('close', code, null)
    input.destroy(); output.destroy(); errors.destroy()
  }
  input.on('data', () => { if (cooperative) queueMicrotask(() => { close() }) })
  class Wsl extends WslHostTransport {
    protected override launch(): ChildProcessWithoutNullStreams { launched++; return child }
    protected override forceStop(): void { forced++; close() }
  }
  class Windows extends WindowsHostTransport {
    protected override launch(): ChildProcessWithoutNullStreams { launched++; return child }
    protected override forceStop(): void { forced++; close() }
  }
  const options = { node: '/fixture/node', entry: '/fixture/host.js', ...callbacks }
  const transport = native ? new Windows({ ...options, cwd: '/fixture' }) : new Wsl({ ...options, distro: 'fixture' })
  const sockets: Socket[] = []
  const attempts = new Map<number, { promise: Promise<Socket>; resolve: (socket: Socket) => void }>()
  const attempt = (index: number) => {
    let value = attempts.get(index)
    if (!value) { value = Promise.withResolvers<Socket>(); attempts.set(index, value) }
    return value
  }
  vi.mocked(createConnection).mockImplementation(() => {
    const socket = new Socket()
    sockets.push(socket)
    attempt(sockets.length).resolve(socket)
    return socket
  })
  onTestFinished(async () => {
    close()
    await transport.stop()
    for (const socket of sockets) socket.destroy()
    vi.useRealTimers()
    vi.mocked(createConnection).mockReset()
  })
  const ready = { type: 'ready', protocol: 1, url: 'http://127.0.0.1:31415/?token=private-fixture', pid: 31415, home: '/fixture/home' }
  return { transport, child, output, errors, sockets, close, forced: () => forced, launched: () => launched,
    socket: (index: number) => attempt(index).promise,
    ready: () => { output.write(`RAINY_CONTROL ${JSON.stringify(ready)}\n`) },
  }
}

function outcome(promise: ReturnType<WslHostTransport['start']>) {
  return promise.then(value => ({ value, error: undefined }), (error: unknown) => ({
    value: undefined, error: error instanceof Error ? error : new Error(String(error)),
  }))
}

describe('Host startup ownership', () => {
  it('answers the Host system proxy questions from its first line, only for web addresses', async ({ onTestFinished }) => {
    const asked: string[] = []
    const test = fixture(onTestFinished, true, false, { resolveProxy: (url) => {
      asked.push(url)
      return url.includes('corp') ? Promise.resolve(null) : url.includes('broken') ? Promise.reject(new Error('no session')) : Promise.resolve('http://127.0.0.1:7890')
    } })
    const replies: unknown[] = []
    test.child.stdin.on('data', (chunk: Buffer) => { for (const line of chunk.toString().split('\n').filter(Boolean)) replies.push(JSON.parse(line)) })
    const ready = test.transport.start()
    const ask = (id: string, url: string): void => { test.output.write(`RAINY_CONTROL ${JSON.stringify({ type: 'system-proxy', id, url })}\n`) }
    ask('a', 'https://api.openai.com/')
    ask('b', 'https://llm.corp/')
    ask('c', 'file:///C:/secret.txt')
    ask('d', 'https://broken.example/')
    await vi.waitFor(() => { expect(replies).toHaveLength(4) })
    expect(replies.toSorted((x, y) => JSON.stringify(x).localeCompare(JSON.stringify(y)))).toEqual([
      { type: 'reply', id: 'a', url: 'http://127.0.0.1:7890' }, { type: 'reply', id: 'b', url: null },
      { type: 'reply', id: 'c', url: null }, { type: 'reply', id: 'd', url: null },
    ])
    expect(asked).toEqual(['https://api.openai.com/', 'https://llm.corp/', 'https://broken.example/'])
    test.ready()
    expect((await ready).pid).toBe(31415)
  })

  it('hands the credential key to the Host, and no key when it cannot be read', async ({ onTestFinished }) => {
    const diagnostics: string[] = []
    let fail = false
    const test = fixture(onTestFinished, true, false, {
      credentialKey: () => fail ? Promise.reject(new Error('userData is read-only')) : Promise.resolve('a2V5'),
      onDiagnostic: (text) => { diagnostics.push(text) },
    })
    const replies: unknown[] = []
    test.child.stdin.on('data', (chunk: Buffer) => { for (const line of chunk.toString().split('\n').filter(Boolean)) replies.push(JSON.parse(line)) })
    // The Host never reports ready here; teardown stops it before readiness.
    void outcome(test.transport.start())
    test.output.write(`RAINY_CONTROL ${JSON.stringify({ type: 'credential-key', id: 'first' })}\n`)
    await vi.waitFor(() => { expect(replies).toEqual([{ type: 'reply', id: 'first', key: 'a2V5' }]) })
    fail = true
    test.output.write(`RAINY_CONTROL ${JSON.stringify({ type: 'credential-key', id: 'second' })}\n`)
    await vi.waitFor(() => { expect(replies).toContainEqual({ type: 'reply', id: 'second', key: null }) })
    expect(diagnostics.some(text => text.includes('unencrypted') && text.includes('read-only'))).toBe(true)
  })

  it('leaves native Windows readiness free of loopback probes', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished, true)
    const ready = test.transport.start()
    expect(test.transport.start()).toBe(ready)
    test.ready()
    expect((await ready).pid).toBe(31415)
    expect(createConnection).not.toHaveBeenCalled()
    await test.transport.stop()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('waits for Windows TCP acceptance after WSL refusal and ignores duplicate ready', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished)
    let settled = false
    const ready = test.transport.start().then((value) => { settled = true; return value })
    test.ready(); test.ready()
    const first = await test.socket(1)
    first.emit('error', Object.assign(new Error('not forwarded yet'), { code: 'ECONNREFUSED' }))
    await vi.advanceTimersByTimeAsync(99)
    expect(settled).toBe(false)
    expect(createConnection).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    const second = await test.socket(2)
    second.emit('connect')
    expect((await ready).pid).toBe(31415)
    expect(first.destroyed && second.destroyed).toBe(true)
    expect(createConnection).toHaveBeenLastCalledWith({ host: '127.0.0.1', port: 31415 })
    expect(vi.getTimerCount()).toBe(0)
  })

  it('stops before stdout readiness without launching a probe or resolving late', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished, false, false)
    const result = outcome(test.transport.start())
    let stopped = false
    const stop = test.transport.stop().then(() => { stopped = true })
    test.ready()
    await Promise.resolve()
    expect(stopped).toBe(false)
    expect(createConnection).not.toHaveBeenCalled()
    test.close()
    await stop
    expect((await result).error?.message).toContain('stopped')
    expect(vi.getTimerCount()).toBe(0)
  })

  it('aborts an active socket and waits for both socket and process close', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished, false, false)
    const result = outcome(test.transport.start())
    test.ready()
    const socket = await test.socket(1)
    let stopped = false
    const stop = test.transport.stop().then(() => { stopped = true })
    await Promise.resolve()
    expect(socket.destroyed).toBe(true)
    expect(stopped).toBe(false)
    test.close()
    await stop
    expect((await result).error).toBeInstanceOf(Error)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('cancels the retry delay without creating another socket', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished)
    const result = outcome(test.transport.start())
    test.ready()
    ;(await test.socket(1)).emit('error', Object.assign(new Error('not forwarded'), { code: 'ECONNREFUSED' }))
    await vi.advanceTimersByTimeAsync(50)
    await test.transport.stop()
    await vi.advanceTimersByTimeAsync(1000)
    expect((await result).error).toBeInstanceOf(Error)
    expect(createConnection).toHaveBeenCalledTimes(1)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('does not resolve readiness when stop races a successful TCP connection', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished)
    const result = outcome(test.transport.start())
    test.ready()
    ;(await test.socket(1)).emit('connect')
    await test.transport.stop()
    expect((await result).error).toBeInstanceOf(Error)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('rejects process failure only after the owned child closes', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished, false, false)
    let settled = false
    const result = outcome(test.transport.start()).then((value) => { settled = true; return value })
    test.ready()
    const socket = await test.socket(1)
    test.child.emit('error', new Error('fixture child failure'))
    await Promise.resolve()
    expect(settled).toBe(false)
    expect(socket.destroyed).toBe(true)
    test.close(1)
    expect((await result).error?.message).toBe('fixture child failure')
    expect(vi.getTimerCount()).toBe(0)
  })

  it('retains the original startup deadline while awaiting forwarding and completes forced cleanup', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished, false, false)
    let settled = false
    const result = outcome(test.transport.start()).then((value) => { settled = true; return value })
    await vi.advanceTimersByTimeAsync(89000)
    test.ready()
    const socket = await test.socket(1)
    await vi.advanceTimersByTimeAsync(1000)
    expect(socket.destroyed).toBe(true)
    expect(settled).toBe(false)
    await vi.advanceTimersByTimeAsync(8000)
    expect((await result).error?.message).toContain('deadline')
    expect(test.forced()).toBe(1)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('does not retry a non-transient connection failure', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished)
    const result = outcome(test.transport.start())
    test.ready()
    ;(await test.socket(1)).emit('error', Object.assign(new Error('fixture denied'), { code: 'EACCES' }))
    expect((await result).error?.message).toBe('fixture denied')
    expect(createConnection).toHaveBeenCalledTimes(1)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('does not launch after a prior stop', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished)
    await test.transport.stop()
    await expect(test.transport.start()).rejects.toThrow('already stopped')
    expect(test.launched()).toBe(0)
  })

  it('contains diagnostic and exit callback exceptions without losing startup or teardown', async ({ onTestFinished }) => {
    const logging = vi.spyOn(console, 'error').mockImplementation(() => {})
    onTestFinished(() => { logging.mockRestore() })
    const test = fixture(onTestFinished, true, true, {
      onDiagnostic: () => { throw new Error('fixture diagnostic callback') },
      onExit: () => { throw new Error('fixture exit callback') },
    })
    const ready = test.transport.start()
    test.errors.write('fixture diagnostic\n')
    test.ready()
    await ready
    await test.transport.stop()
    expect(logging).toHaveBeenCalledTimes(2)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('rejects a synchronous launch failure without leaving timers or probes', async ({ onTestFinished }) => {
    const test = fixture(onTestFinished)
    class FailedLaunch extends WslHostTransport {
      protected override launch(): ChildProcessWithoutNullStreams { throw new Error('fixture launch failure') }
    }
    const failed = new FailedLaunch({ node: '/fixture/node', entry: '/fixture/host.js', distro: 'fixture' })
    await expect(failed.start()).rejects.toThrow('fixture launch failure')
    await failed.stop()
    expect(test.launched()).toBe(0)
    expect(createConnection).not.toHaveBeenCalled()
    expect(vi.getTimerCount()).toBe(0)
  })
})
