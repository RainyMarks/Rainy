/** WebSocket client for the Host's `/rpc` endpoint, with automatic reconnection. */
import { useSyncExternalStore } from 'react'
import type {
  HostEvent, HostEvents, HostMethod, MethodParams, MethodResult, RpcErrorBody, RpcEventFrame, RpcResponseFrame,
} from '../shared/rpc.ts'

/** A failed Host request. */
export class HostError extends Error {
  readonly code: string
  readonly data: RpcErrorBody['data']
  /** @param body Error body from the Host. */
  constructor(body: RpcErrorBody) {
    super(body.message)
    this.name = 'HostError'
    this.code = body.code
    this.data = body.data
  }
}

export type ConnectionState = 'connecting' | 'open' | 'closed'

interface Pending { resolve(value: unknown): void; reject(error: Error): void }

class HostClient {
  private socket: WebSocket | undefined
  private nextId = 1
  private readonly pending = new Map<number, Pending>()
  private readonly waiting: { id: number; frame: string }[] = []
  private readonly sent = new Set<number>()
  private readonly listeners = new Map<HostEvent, Set<(data: never) => void>>()
  private readonly stateListeners = new Set<() => void>()
  private reconnectDelay = 250
  state: ConnectionState = 'connecting'

  constructor() {
    if (typeof window !== 'undefined' && typeof WebSocket !== 'undefined') this.connect()
  }

  /**
   * Call a Host method.
   * @param method Method name.
   * @param params Method parameters.
   * @returns The result; rejects with {@link HostError}.
   */
  call<M extends HostMethod>(method: M, ...params: MethodParams<M> extends void ? [] : [MethodParams<M>]): Promise<MethodResult<M>> {
    const id = this.nextId++
    const frame = JSON.stringify({ id, method, params: params[0] ?? null })
    return new Promise<MethodResult<M>>((resolve, reject) => {
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject })
      if (this.socket?.readyState === WebSocket.OPEN) { this.socket.send(frame); this.sent.add(id) }
      else this.waiting.push({ id, frame })
    })
  }

  /**
   * Listen to a Host event.
   * @param event Event name.
   * @param listener Receives the payload.
   * @returns A function that removes the listener.
   */
  on<E extends HostEvent>(event: E, listener: (data: HostEvents[E]) => void): () => void {
    let set = this.listeners.get(event)
    if (set === undefined) { set = new Set(); this.listeners.set(event, set) }
    set.add(listener as (data: never) => void)
    return () => { set.delete(listener as (data: never) => void) }
  }

  /**
   * Observe connection state changes.
   * @param listener Called after each change.
   * @returns A function that removes the listener.
   */
  onState(listener: () => void): () => void {
    this.stateListeners.add(listener)
    return () => { this.stateListeners.delete(listener) }
  }

  private setState(state: ConnectionState): void {
    this.state = state
    for (const listener of this.stateListeners) listener()
  }

  private connect(): void {
    const url = new URL('/rpc', window.location.href)
    url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
    const socket = new WebSocket(url)
    this.socket = socket
    this.setState('connecting')
    socket.addEventListener('open', () => {
      this.reconnectDelay = 250
      this.setState('open')
      for (const { id, frame } of this.waiting.splice(0)) { socket.send(frame); this.sent.add(id) }
    })
    socket.addEventListener('message', (message) => { this.receive(String(message.data)) })
    socket.addEventListener('close', () => {
      if (this.socket !== socket) return
      this.socket = undefined
      this.setState('closed')
      // Requests sent on the lost connection never get answers.
      for (const id of this.sent) {
        this.pending.get(id)?.reject(new HostError({ code: 'disconnected', message: 'Connection to RainyAgent was lost.' }))
        this.pending.delete(id)
      }
      this.sent.clear()
      setTimeout(() => { this.connect() }, this.reconnectDelay)
      this.reconnectDelay = Math.min(this.reconnectDelay * 2, 5000)
    })
  }

  private receive(text: string): void {
    let frame: RpcResponseFrame | RpcEventFrame
    try { frame = JSON.parse(text) as RpcResponseFrame | RpcEventFrame } catch (_error) { return }
    if ('event' in frame) {
      for (const listener of this.listeners.get(frame.event) ?? []) {
        try { (listener as (data: unknown) => void)(frame.data) } catch (error) { console.error(error) }
      }
      return
    }
    const pending = this.pending.get(frame.id)
    if (pending === undefined) return
    this.pending.delete(frame.id)
    this.sent.delete(frame.id)
    if (frame.error !== undefined) pending.reject(new HostError(frame.error))
    else pending.resolve(frame.result)
  }
}

/** The renderer's single Host connection. */
export const host = new HostClient()

/** @returns The current connection state; re-renders on change. */
export function useConnectionState(): ConnectionState {
  return useSyncExternalStore(listener => host.onState(listener), () => host.state)
}
