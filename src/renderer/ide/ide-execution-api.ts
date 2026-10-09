/** Terminal, run and debug requests over the Host's `ide` RPC method. */
import type { IdeExecutionRequest, IdeExecutionResponseMap } from '../../shared/ide-execution-protocol.ts'
import { callIde } from './ide-api.ts'

/** Operation-specific typed API consumed by the execution model; tests substitute their own. */
export interface IdeExecutionApi {
  /** @param request Human operation. @param signal Optional cancellation. @returns Its result. */
  request<K extends IdeExecutionRequest['op']>(
    request: Extract<IdeExecutionRequest, { op: K }>, signal?: AbortSignal,
  ): Promise<IdeExecutionResponseMap[K]>
}

/** @returns The execution adapter backed by the Host connection. */
export function createIdeExecutionApi(): IdeExecutionApi {
  return {
    request: async <K extends IdeExecutionRequest['op']>(request: Extract<IdeExecutionRequest, { op: K }>, signal?: AbortSignal) =>
      await callIde(request, signal) as IdeExecutionResponseMap[K],
  }
}
