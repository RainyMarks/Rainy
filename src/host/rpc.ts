/** JSON-RPC over the renderer WebSocket: method table and event broadcast. */
import type { WebSocket } from 'ws'
import type {
  HostEvent, HostEvents, HostMethod, MethodParams, MethodResult, RpcErrorBody, RpcEventFrame, RpcRequestFrame, RpcResponseFrame,
} from '../shared/rpc.ts'

/** A failure reported to the renderer with a stable code. */
export class RpcError extends Error {
  /**
   * @param code Stable machine-readable code.
   * @param message Human-readable message, already localized where it reaches the user.
   * @param data Extra fields the caller needs, such as an IDE conflict observation.
   */
  constructor(readonly code: string, message: string, readonly data?: RpcErrorBody['data']) {
    super(message)
  }
}

type Handler<M extends HostMethod> = (params: MethodParams<M>) => Promise<MethodResult<M>> | MethodResult<M>

const MAX_FRAME_BYTES = 64 * 1024 * 1024

/** Routes renderer requests to registered handlers and broadcasts Host events. */
export class RpcHub {
  private readonly handlers = new Map<HostMethod, Handler<HostMethod>>()
  private readonly sockets = new Set<WebSocket>()

  /**
   * Register the handler for one method.
   * @param method Method name from `HostMethods`.
   * @param handler Receives the request params and returns the result.
   * @throws Error when the method already has a handler.
   */
  register<M extends HostMethod>(method: M, handler: Handler<M>): void {
    if (this.handlers.has(method)) throw new Error(`RPC method ${method} is already registered`)
    this.handlers.set(method, handler as unknown as Handler<HostMethod>)
  }

  /**
   * Send an event to every connected renderer.
   * @param event Event name from `HostEvents`.
   * @param data Event payload.
   */
  emit<E extends HostEvent>(event: E, data: HostEvents[E]): void {
    if (this.sockets.size === 0) return
    const frame: RpcEventFrame = { event, data }
    const text = JSON.stringify(frame)
    for (const socket of this.sockets) if (socket.readyState === socket.OPEN) socket.send(text)
  }

  /**
   * Serve one authenticated renderer connection until it closes.
   * @param socket Upgraded WebSocket.
   */
  attach(socket: WebSocket): void {
    this.sockets.add(socket)
    socket.on('close', () => { this.sockets.delete(socket) })
    socket.on('error', () => { this.sockets.delete(socket) })
    socket.on('message', (raw, binary) => {
      if (binary) return
      const text = raw.toString()
      if (text.length > MAX_FRAME_BYTES) { socket.close(1009, 'frame too large'); return }
      let frame: RpcRequestFrame
      try {
        const parsed: unknown = JSON.parse(text)
        if (parsed === null || typeof parsed !== 'object' || !('id' in parsed) || typeof parsed.id !== 'number'
          || !('method' in parsed) || typeof parsed.method !== 'string') throw new Error('invalid frame')
        frame = parsed as RpcRequestFrame
      } catch (_error) {
        socket.close(1003, 'invalid frame')
        return
      }
      void this.dispatch(frame).then((response) => {
        if (socket.readyState === socket.OPEN) socket.send(JSON.stringify(response))
      })
    })
  }

  /**
   * Run one request through its handler; also used by in-process callers and tests.
   * @param frame Request frame.
   * @returns The response frame; failures are encoded, never thrown.
   */
  async dispatch(frame: RpcRequestFrame): Promise<RpcResponseFrame> {
    const handler = this.handlers.get(frame.method)
    if (handler === undefined) return { id: frame.id, error: { code: 'unknown-method', message: `Unknown method ${frame.method}` } }
    try {
      const result = await handler(frame.params as never)
      return { id: frame.id, result: result === undefined ? null : result }
    } catch (error) {
      return { id: frame.id, error: toErrorBody(error) }
    }
  }

  /** Close every renderer connection. */
  close(): void {
    for (const socket of this.sockets) socket.close(1001, 'host stopping')
    this.sockets.clear()
  }
}

/**
 * Convert a thrown value to the wire error body.
 * @param error Thrown value.
 * @returns The code, message and optional data sent to the renderer.
 */
export function toErrorBody(error: unknown): RpcErrorBody {
  if (error instanceof RpcError) return { code: error.code, message: error.message, ...(error.data === undefined ? {} : { data: error.data }) }
  return { code: 'error', message: error instanceof Error ? error.message : String(error) }
}
