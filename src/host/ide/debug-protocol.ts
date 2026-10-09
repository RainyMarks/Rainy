/** Request correlation and reverse requests over a bounded DAP byte stream. */
import type { Readable, Writable } from 'node:stream'
import { z } from 'zod'
import { assertNever } from '../../shared/brand.ts'
import { encodeMessage, MessageDecoder } from './jsonrpc-framing.ts'

const responseSchema = z
  .object({
    type: z.literal('response'),
    request_seq: z.number().int(),
    command: z.string(),
    success: z.boolean(),
    message: z.string().optional(),
    body: z.unknown().optional(),
  })
  .loose()
const eventSchema = z.object({ type: z.literal('event'), event: z.string(), body: z.unknown().optional() }).loose()
const requestSchema = z
  .object({ type: z.literal('request'), seq: z.number().int(), command: z.string(), arguments: z.unknown().optional() })
  .loose()
const messageSchema = z.discriminatedUnion('type', [responseSchema, eventSchema, requestSchema])

/** Owner callbacks and bounds for one debug-adapter connection. */
export interface IdeDapPeerOptions {
  readonly input: Readable
  readonly output: Writable
  readonly maxMessageBytes: number
  readonly requestTimeoutMs: number
  readonly signal: AbortSignal
  readonly onEvent: (event: string, body: unknown) => void
  readonly onRequest: (command: string, arguments_: unknown) => Promise<unknown>
  readonly onFailure: (error: Error) => void
  readonly reportCallbackError: (error: unknown) => void
}

interface PendingRequest {
  command: string
  resolve: (body: unknown) => void
  reject: (error: Error) => void
  timer: ReturnType<typeof setTimeout>
}

function errorOf(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error))
}

/** One DAP stream; closing rejects outstanding requests and drains reverse-request handlers. */
export class IdeDapPeer {
  private readonly decoder: MessageDecoder
  private readonly pending = new Map<number, PendingRequest>()
  private readonly reverse = new Set<Promise<void>>()
  private sequence = 0
  private closed = false
  private initializedResolve!: () => void
  private initializedReject!: (error: Error) => void
  private initializationCount = 0
  private readonly initializationWaiters = new Set<{ after: number; resolve: () => void; reject: (error: Error) => void }>()
  /** Resolves on the adapter's initialized event; fails with this connection. */
  readonly initialized: Promise<void>
  /** Number of initialized events observed on this transport. */
  get initializationSequence(): number {
    return this.initializationCount
  }

  /**
   * Wait for a target initialized after a bootstrap handshake.
   * @param after - previously observed initialization sequence.
   * @returns the next initialization or connection failure.
   */
  initializedAfter(after: number): Promise<void> {
    if (this.closed) return Promise.reject(new Error('The debug adapter connection is closed.'))
    if (this.initializationCount > after) return Promise.resolve()
    return new Promise<void>((resolve, reject) => {
      this.initializationWaiters.add({ after, resolve, reject })
    })
  }

  /** @param options - owned transport, limits, cancellation, and callbacks. */
  constructor(private readonly options: IdeDapPeerOptions) {
    this.decoder = new MessageDecoder(options.maxMessageBytes)
    this.initialized = new Promise<void>((resolve, reject) => {
      this.initializedResolve = resolve
      this.initializedReject = reject
    })
    void this.initialized.catch(() => {
      /* The owner may close before beginning initialization. */
    })
    options.input.on('data', this.onData)
    options.input.once('end', this.onEnd)
    options.input.once('error', this.onError)
    options.output.on('error', this.onError)
    options.signal.addEventListener('abort', this.onAbort, { once: true })
    if (options.signal.aborted) this.onAbort()
  }

  /**
   * Send one allowed owner operation.
   * @param command - DAP command.
   * @param arguments_ - structured DAP arguments.
   * @returns its response body.
   */
  request(command: string, arguments_: unknown): Promise<unknown> {
    if (this.closed) return Promise.reject(new Error('The debug adapter connection is closed.'))
    const sequence = ++this.sequence
    return new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(sequence)
        reject(new Error(`The debug adapter did not answer ${command} before its deadline.`))
      }, this.options.requestTimeoutMs)
      timer.unref()
      this.pending.set(sequence, { command, resolve, reject, timer })
      this.write({ seq: sequence, type: 'request', command, arguments: arguments_ })
    })
  }

  /**
   * Stop callbacks and reject requests before the owner stops adapter processes.
   * @param reason - diagnostic for in-flight callers.
   * @returns completion after reverse-request handlers settle.
   */
  async close(reason = new Error('The debug adapter connection was closed.')): Promise<void> {
    this.finish(reason, false)
    await Promise.allSettled([...this.reverse])
  }

  private readonly onAbort = (): void => {
    this.finish(new Error('Debugging was stopped.'), false)
  }
  private readonly onEnd = (): void => {
    this.finish(new Error('The debug adapter closed its output stream.'), true)
  }
  private readonly onError = (error: Error): void => {
    this.finish(error, true)
  }
  private readonly onData = (chunk: unknown): void => {
    if (this.closed) return
    try {
      if (!Buffer.isBuffer(chunk) && typeof chunk !== 'string') throw new Error('The debug adapter returned a non-byte stream.')
      for (const value of this.decoder.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk))) this.receive(value)
    } catch (error) {
      this.finish(errorOf(error), true)
    }
  }

  private write(message: unknown): void {
    if (this.closed) return
    let data: Buffer
    try {
      data = encodeMessage(message)
    } catch (error) {
      this.finish(errorOf(error), true)
      return
    }
    if (data.length > this.options.maxMessageBytes) {
      this.finish(new Error('A debug request exceeds the configured message size limit.'), true)
      return
    }
    try {
      this.options.output.write(data, (error) => {
        if (error) this.finish(error, true)
      })
    } catch (error) {
      this.finish(errorOf(error), true)
    }
  }

  private receive(value: unknown): void {
    const message = messageSchema.parse(value)
    switch (message.type) {
      case 'response': {
        const pending = this.pending.get(message.request_seq)
        if (!pending) return
        this.pending.delete(message.request_seq)
        clearTimeout(pending.timer)
        if (message.command !== pending.command) {
          pending.reject(new Error('The debug adapter answered another command.'))
          return
        }
        if (message.success) pending.resolve(message.body)
        else pending.reject(new Error(message.message ?? `The debug adapter rejected ${message.command}.`))
        return
      }
      case 'event': {
        if (message.event === 'initialized') {
          this.initializationCount++
          this.initializedResolve()
          for (const waiter of this.initializationWaiters) {
            if (this.initializationCount > waiter.after) {
              this.initializationWaiters.delete(waiter)
              waiter.resolve()
            }
          }
        }
        try {
          this.options.onEvent(message.event, message.body)
        } catch (error) {
          this.finish(errorOf(error), true)
        }
        return
      }
      case 'request': {
        const handler = this.reverseRequest(message.seq, message.command, message.arguments)
        this.reverse.add(handler)
        void handler.finally(() => {
          this.reverse.delete(handler)
        })
        return
      }
      default:
        return assertNever(message)
    }
  }

  private async reverseRequest(sequence: number, command: string, arguments_: unknown): Promise<void> {
    try {
      const body = await this.options.onRequest(command, arguments_)
      this.write({ seq: ++this.sequence, type: 'response', request_seq: sequence, command, success: true, body })
    } catch (error) {
      this.write({
        seq: ++this.sequence,
        type: 'response',
        request_seq: sequence,
        command,
        success: false,
        message: errorOf(error).message,
      })
    }
  }

  private finish(error: Error, notify: boolean): void {
    if (this.closed) return
    this.closed = true
    this.options.input.off('data', this.onData)
    this.options.input.off('end', this.onEnd)
    this.options.input.once('close', () => {
      this.options.input.off('error', this.onError)
    })
    this.options.output.once('close', () => {
      this.options.output.off('error', this.onError)
    })
    this.options.signal.removeEventListener('abort', this.onAbort)
    this.initializedReject(error)
    for (const waiter of this.initializationWaiters) waiter.reject(error)
    this.initializationWaiters.clear()
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer)
      pending.reject(error)
    }
    this.pending.clear()
    if (notify) {
      try {
        this.options.onFailure(error)
      } catch (callbackError) {
        this.options.reportCallbackError(callbackError)
      }
    }
  }
}
