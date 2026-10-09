/** Replacement for `src/renderer/rpc.ts` in renderer tests: no socket; `host.call` is a spy that never settles unless a test says so. */
import { vi } from 'vitest'
import type { HostMethod, RpcErrorBody } from '../../src/shared/rpc.ts'

/** Same fields as the real `HostError`. */
export class HostError extends Error {
  readonly code: string
  readonly data: RpcErrorBody['data']
  /** @param body Error body. */
  constructor(body: RpcErrorBody) {
    super(body.message)
    this.name = 'HostError'
    this.code = body.code
    this.data = body.data
  }
}

/** Spy-backed stand-in for the Host connection. */
export const host = {
  state: 'open' as const,
  call: vi.fn<(method: HostMethod, params?: unknown) => Promise<unknown>>(() => new Promise(() => undefined)),
  on: vi.fn(() => () => undefined),
  onState: vi.fn(() => () => undefined),
}

/** @returns Always open. */
export function useConnectionState(): 'open' {
  return 'open'
}
