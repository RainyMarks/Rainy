/** IDE file, state, formatting and execution requests over the Host's `ide` RPC method. */
import type { IdeRequest, IdeResult } from '../../shared/rpc.ts'
import type {
  IdeFilesRequest, IdeFilesResults, IdeRootId, IdeStateRequest, IdeWorkspaceState, WorkspaceId,
} from '../../shared/ide-files-protocol.ts'
import { host, HostError } from '../rpc.ts'

/** Formatter request; the path only names the file for the formatter's language detection. */
export interface IdeFormatRequest {
  readonly op: 'format'
  readonly workspaceId: WorkspaceId
  readonly rootId?: IdeRootId | undefined
  readonly path: string
  readonly text: string
  readonly language: string
}

/** Results accepted by the editor's file/state adapter. */
export type IdeApiResults = IdeFilesResults & { format: { readonly text: string } }
/** Requests accepted by the editor's file/state adapter. */
export type IdeApiRequest = IdeFilesRequest | IdeStateRequest | IdeFormatRequest

/** A failed IDE operation with the Host's stable error code and, for recovery conflicts, the current state. */
export class IdeRequestError extends Error {
  /**
   * @param code Stable Host error code (`IdeFilesErrorCode`, or `disconnected` when the connection dropped).
   * @param message Host diagnostic.
   * @param currentState Conflicting durable workspace state.
   */
  constructor(
    readonly code: string,
    message: string,
    readonly currentState?: IdeWorkspaceState,
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
    if (error instanceof HostError) throw new IdeRequestError(error.code, error.message, error.data?.currentState)
    throw error
  }
}

/** Typed file/state adapter; tests substitute their own. */
export interface IdeFilesApi {
  /** @param request Workspace operation. @param signal Optional cancellation. @returns Its result. */
  request<K extends IdeApiRequest['op']>(request: Extract<IdeApiRequest, { op: K }>, signal?: AbortSignal): Promise<IdeApiResults[K]>
}

/** @returns The file/state adapter backed by the Host connection. */
export function createIdeFilesApi(): IdeFilesApi {
  return {
    request: async <K extends IdeApiRequest['op']>(request: Extract<IdeApiRequest, { op: K }>, signal?: AbortSignal) =>
      // The formatter request without `rootId` is the wire request; every other request is a wire request as is.
      await callIde(request as IdeRequest, signal) as IdeApiResults[K],
  }
}
