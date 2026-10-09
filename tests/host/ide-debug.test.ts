/** DAP transport, Content-Length framing and an end-to-end native debug session. */
import { mkdtemp, realpath, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { PassThrough } from 'node:stream'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type {
  IdeDebugFrame, IdeDebugScope, IdeDebugSnapshot, IdeDebugVariable, IdeExecutionPoll, IdeExecutionStatus,
} from '../../src/shared/ide-execution-protocol.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { resolveExecutable } from '../../src/host/process.ts'
import { IdeDapPeer } from '../../src/host/ide/debug-protocol.ts'
import { createIdeExecutionService } from '../../src/host/ide/execution.ts'
import { localIdeSubprocess } from '../../src/host/ide/execution-process.ts'
import { ideExecutionLimitsSchema } from '../../src/host/ide/execution-schema.ts'
import { encodeMessage, MessageDecoder } from '../../src/host/ide/jsonrpc-framing.ts'

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
  output.on('data', (chunk: Buffer) => { messages.push(...decoder.push(chunk)) })
  const onFailure = vi.fn()
  const onEvent = vi.fn()
  const peer = new IdeDapPeer({ input, output, signal: controller.signal, maxMessageBytes: 4096, requestTimeoutMs: 100,
    onEvent, onRequest, onFailure, reportCallbackError: vi.fn() })
  cleanups.push(async () => {
    await peer.close()
    input.destroy()
    output.destroy()
  })
  return { peer, input, output, controller, messages, onFailure, onEvent }
}

describe('Content-Length framing', () => {
  it('round-trips UTF-8 messages split at any byte and ignores other headers', () => {
    const decoder = new MessageDecoder(1024)
    const frame = Buffer.concat([Buffer.from('Content-Type: application/vscode-jsonrpc\r\n'), encodeMessage({ text: '雨天' })])
    const received: unknown[] = []
    for (let index = 0; index < frame.length; index++) received.push(...decoder.push(frame.subarray(index, index + 1)))
    expect(received).toEqual([{ text: '雨天' }])
    expect(decoder.push(Buffer.concat([encodeMessage(1), encodeMessage(2)]))).toEqual([1, 2])
  })

  it('rejects oversized, malformed and header-less frames', () => {
    expect(() => new MessageDecoder(4).push(encodeMessage('too long'))).toThrow('exceeds')
    expect(() => new MessageDecoder(64).push(Buffer.from('Content-Length: x\r\n\r\n'))).toThrow('invalid Content-Length')
    expect(() => new MessageDecoder(64).push(Buffer.from('X: 1\r\n\r\n'))).toThrow('missing Content-Length')
    expect(() => new MessageDecoder(64).push(Buffer.from('Content-Length: 2\r\n\r\n{]'))).toThrow('not valid JSON')
    expect(() => new MessageDecoder(64).push(Buffer.alloc(70000, 65))).toThrow('without a terminator')
  })
})

describe('owned DAP streams', () => {
  it('correlates out-of-order responses and accepts frames split across byte chunks', async () => {
    const f = fixture()
    const first = f.peer.request('threads', {})
    const second = f.peer.request('stackTrace', { threadId: 7 })
    const frame = encodeMessage({ seq: 4, type: 'response', request_seq: 2, command: 'stackTrace', success: true, body: { stackFrames: [] } })
    f.input.write(frame.subarray(0, 8))
    f.input.write(frame.subarray(8))
    f.input.write(encodeMessage({ seq: 5, type: 'response', request_seq: 1, command: 'threads', success: true, body: { threads: [{ id: 7, name: 'main' }] } }))
    await expect(second).resolves.toEqual({ stackFrames: [] })
    await expect(first).resolves.toEqual({ threads: [{ id: 7, name: 'main' }] })
    expect(f.onFailure).not.toHaveBeenCalled()
  })

  it('rejects a timed-out request without misassigning its late response', async () => {
    vi.useFakeTimers()
    const f = fixture()
    const rejected = expect(f.peer.request('threads', {})).rejects.toThrow('deadline')
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
    const target = f.peer.initializedAfter(f.peer.initializationSequence).then(() => { initialized = true })
    await Promise.resolve()
    expect(initialized).toBe(false)
    f.input.write(encodeMessage({ type: 'event', event: 'initialized' }))
    await target
    expect(f.peer.initializationSequence).toBe(2)
  })

  it('waits for admitted reverse handlers and suppresses late responses after close', async () => {
    const gate = Promise.withResolvers<unknown>()
    const f = fixture(async () => gate.promise)
    f.input.write(encodeMessage({ type: 'request', seq: 10, command: 'runInTerminal', arguments: {} }))
    let closed = false
    const close = f.peer.close().then(() => { closed = true })
    await Promise.resolve()
    expect(closed).toBe(false)
    gate.resolve({ processId: 5 })
    await close
    expect(f.messages).toEqual([])
  })

  it('rejects pending requests on cancellation and contains late transport errors', async () => {
    const f = fixture()
    const rejected = expect(f.peer.request('initialize', {})).rejects.toThrow('stopped')
    f.controller.abort()
    await rejected
    expect(() => { f.output.emit('error', new Error('late EPIPE')) }).not.toThrow()
    expect(f.onFailure).not.toHaveBeenCalled()
  })

  it('fails oversized incoming frames before processing events', () => {
    const f = fixture()
    f.input.write(Buffer.from('Content-Length: 100000\r\n\r\n'))
    expect(f.onFailure).toHaveBeenCalledTimes(1)
    expect(f.onEvent).not.toHaveBeenCalled()
  })
})

const gdb = process.platform === 'win32' ? undefined : resolveExecutable('gdb')
const gcc = process.platform === 'win32' ? undefined : resolveExecutable('gcc')

describe.skipIf(gdb === undefined || gcc === undefined)('native debugging with GDB', () => {
  // GDB 15 starts the program at `launch`, so this flow configures breakpoints only after the entry stop.
  it('builds, stops on entry, sets a breakpoint, reads variables, continues and terminates', async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), 'rainy-ide-debug-')))
    cleanups.push(() => rm(root, { recursive: true, force: true }))
    await writeFile(join(root, 'main.c'), ['#include <stdio.h>', 'int main(void) {', '  int answer = 42;', '  printf("answer %d\\n", answer);', '  return 0;', '}', ''].join('\n'))
    const workspaceId = brandString<WorkspaceId>('debug')
    const service = createIdeExecutionService({
      subprocess: localIdeSubprocess, assertUsable: () => {}, reportError: () => {},
      resources: { resourceRoot: join(root, 'resources'), node: process.execPath, tsxImport: join(root, 'tsx.mjs') },
      resolveWorkspace: async id => ({ workspaceId: id, root }),
      limits: ideExecutionLimitsSchema.parse({}),
    })
    cleanups.push(() => service.dispose())
    const started = await service.handle({ op: 'debug.start', workspaceId, breakpoints: [], stopOnEntry: true,
      configuration: { name: 'native', language: 'c', program: 'main.c', terminal: false } }) as IdeDebugSnapshot
    const debugId = started.id
    const debug = async (): Promise<IdeDebugSnapshot | undefined> =>
      ((await service.handle({ op: 'execution.status', workspaceId })) as IdeExecutionStatus).debugSessions.find(value => value.id === debugId)
    const pausedAt = async (line: number): Promise<number> => {
      await vi.waitFor(async () => {
        const snapshot = await debug()
        if (snapshot?.phase === 'failed') throw new Error(`Debugging failed: ${snapshot.error ?? ''}`)
        expect(snapshot?.phase).toBe('paused')
      }, { timeout: 30000, interval: 100 })
      const threadId = (await debug())?.threadId ?? 1
      const frames = await service.handle({ op: 'debug.stack', workspaceId, debugId, threadId }) as readonly IdeDebugFrame[]
      expect(frames[0]?.line).toBe(line)
      return threadId
    }
    const entry = await pausedAt(3)
    const verified = await service.handle({ op: 'debug.setBreakpoints', workspaceId, debugId, source: { path: 'main.c', lines: [4] } })
    expect(verified).toEqual([expect.objectContaining({ path: join(root, 'main.c'), requestedLine: 4, verified: true })])
    await service.handle({ op: 'debug.control', workspaceId, debugId, action: 'continue', threadId: entry })
    const threadId = await pausedAt(4)
    const frames = await service.handle({ op: 'debug.stack', workspaceId, debugId, threadId }) as readonly IdeDebugFrame[]
    const scopes = await service.handle({ op: 'debug.scopes', workspaceId, debugId, frameId: frames[0]!.id }) as readonly IdeDebugScope[]
    const variables = await service.handle({ op: 'debug.variables', workspaceId, debugId, variablesReference: scopes[0]!.variablesReference }) as readonly IdeDebugVariable[]
    expect(variables.find(variable => variable.name === 'answer')?.value).toBe('42')
    expect(await service.handle({ op: 'debug.evaluate', workspaceId, debugId, expression: 'answer + 1', frameId: frames[0]!.id, context: 'watch' }))
      .toMatchObject({ result: '43' })
    await service.handle({ op: 'debug.control', workspaceId, debugId, action: 'continue', threadId })
    await vi.waitFor(async () => { expect((await debug())?.phase).toBe('terminated') }, { timeout: 30000, interval: 100 })
    await vi.waitFor(() => { expect(service.hasActivity()).toBe(false) })
  }, 60000)

  it.skipIf(resolveExecutable('python3') === undefined)('runs the debuggee in an owned terminal and forwards its input', async () => {
    const root = await realpath(await mkdtemp(join(tmpdir(), 'rainy-ide-debug-tty-')))
    cleanups.push(() => rm(root, { recursive: true, force: true }))
    await writeFile(join(root, 'main.c'), ['#include <stdio.h>', 'int main(void) {', '  char line[32] = {0};', '  if (!fgets(line, sizeof line, stdin)) return 2;', '  printf("echo:%s", line);', '  return 0;', '}', ''].join('\n'))
    const workspaceId = brandString<WorkspaceId>('debug-tty')
    const service = createIdeExecutionService({
      subprocess: localIdeSubprocess, assertUsable: () => {}, reportError: () => {},
      resources: { resourceRoot: join(root, 'resources'), node: process.execPath, tsxImport: join(root, 'tsx.mjs') },
      resolveWorkspace: async id => ({ workspaceId: id, root }),
      limits: ideExecutionLimitsSchema.parse({}),
    })
    cleanups.push(() => service.dispose())
    const started = await service.handle({ op: 'debug.start', workspaceId, breakpoints: [],
      configuration: { name: 'native tty', language: 'c', program: 'main.c' } }) as IdeDebugSnapshot
    const output = async (): Promise<string> => (await service.handle({ op: 'execution.poll', workspaceId, cursor: 0 }) as IdeExecutionPoll)
      .events.map(event => event.kind === 'output' && event.stream === 'terminal' ? event.text : '').join('')
    const phase = async (): Promise<string | undefined> => {
      const snapshot = ((await service.handle({ op: 'execution.status', workspaceId })) as IdeExecutionStatus).debugSessions.find(value => value.id === started.id)
      if (snapshot?.phase === 'failed') throw new Error(`Debugging failed: ${snapshot.error ?? ''}`)
      return snapshot?.phase
    }
    await vi.waitFor(async () => { expect(await phase()).toBe('running') }, { timeout: 30000, interval: 100 })
    await service.handle({ op: 'debug.input', workspaceId, debugId: started.id, data: 'typed\r' })
    await vi.waitFor(async () => { expect(await output()).toContain('echo:typed') }, { timeout: 30000, interval: 100 })
    await vi.waitFor(async () => { expect(await phase()).toBe('terminated') }, { timeout: 30000, interval: 100 })
  }, 60000)
})
