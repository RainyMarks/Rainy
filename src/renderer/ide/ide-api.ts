/** The Host's `ide` RPC method, for the file requests the viewer pages make. */
import type { IdeRequest, IdeResult } from '../../shared/rpc.ts'
import { host, HostError } from '../rpc.ts'

/** A failed IDE operation with the Host's stable error code. */
export class IdeRequestError extends Error {
  /**
   * @param code Stable Host error code (`already-exists`, `not-found`…, or `disconnected` when the connection dropped).
   * @param message Host diagnostic.
   */
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message)
    this.name = 'IdeRequestError'
  }
}

function aborted(signal: AbortSignal): Error {
  return signal.reason instanceof Error ? signal.reason : new DOMException('The IDE request was cancelled.', 'AbortError')
}

/**
 * Send one IDE operation. A cancelled request rejects at once; the Host may still complete it, and its result is discarded.
 * @param request IDE operation.
 * @param signal Optional cancellation.
 * @returns The operation result; rejects with {@link IdeRequestError} when the Host refuses it.
 */
export async function callIde<T extends IdeRequest>(request: T, signal?: AbortSignal): Promise<IdeResult<T>> {
  if (signal?.aborted) throw aborted(signal)
  const call = host.call('ide', request) as Promise<IdeResult<T>>
  try {
    if (signal === undefined) return await call
    return await new Promise<IdeResult<T>>((resolve, reject) => {
      const cancel = (): void => { reject(aborted(signal)) }
      signal.addEventListener('abort', cancel, { once: true })
      call.then(resolve, reject).finally(() => { signal.removeEventListener('abort', cancel) })
    })
  } catch (error) {
    if (error instanceof HostError) throw new IdeRequestError(error.code, error.message)
    throw error
  }
}
