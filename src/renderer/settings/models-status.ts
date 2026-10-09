/** Model settings read from the Host and kept current through `models.changed`. */
import { useCallback, useEffect, useRef, useState } from 'react'
import type { ModelsStatus } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { errorText } from './parts.tsx'

/** Latest model settings and the operations that replace them. */
export interface ModelsStatusView {
  status: ModelsStatus | undefined
  /** Failure of the latest read; cleared by the next successful one. */
  error: string
  /**
   * Read the status again. A read that resolves after a newer status was published is not shown.
   * @returns The status the Host returned, or `undefined` after a failure.
   */
  refresh(): Promise<ModelsStatus | undefined>
  /**
   * Publish a status returned by a mutating method.
   * @param status The Host's status after the change.
   */
  accept(status: ModelsStatus): void
}

/** @returns Model settings; reads on mount and after the connection reopens. */
export function useModelsStatus(): ModelsStatusView {
  const [state, setState] = useState<{ status: ModelsStatus | undefined; error: string }>({ status: undefined, error: '' })
  const order = useRef({ issued: 0, shown: 0, mounted: true })
  const accept = useCallback((status: ModelsStatus): void => {
    const current = order.current
    current.shown = ++current.issued
    if (current.mounted) setState({ status, error: '' })
  }, [])
  const refresh = useCallback(async (): Promise<ModelsStatus | undefined> => {
    const current = order.current
    const request = ++current.issued
    try {
      const status = await host.call('models.status')
      if (request > current.shown) {
        current.shown = request
        if (current.mounted) setState({ status, error: '' })
      }
      return status
    } catch (error) {
      if (request > current.shown && current.mounted) setState(previous => ({ ...previous, error: errorText(error) }))
      return undefined
    }
  }, [])
  useEffect(() => {
    order.current.mounted = true
    void refresh()
    const stopChanges = host.on('models.changed', accept)
    const stopState = host.onState(() => { if (host.state === 'open') void refresh() })
    return () => { order.current.mounted = false; stopChanges(); stopState() }
  }, [refresh, accept])
  return { ...state, refresh, accept }
}
