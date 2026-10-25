/** Ctrl+wheel and Ctrl+= / Ctrl+- / Ctrl+0 for the zoom of a viewer: WebView2's own page zoom is off (`webview.rs`),
 *  so the keys reach the page, and a viewer that zooms its content takes them. */
import { useEffect, useRef } from 'react'

/** What the keys do; the wheel passes the pointer position, in client pixels, to zoom around. */
export interface ZoomKeys {
  zoomIn(clientX?: number, clientY?: number): void
  zoomOut(clientX?: number, clientY?: number): void
  reset(): void
}

/** The zoom a key press asks for, or `undefined` for any other key. */
export function zoomKey(event: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'metaKey' | 'altKey'>): 'in' | 'out' | 'reset' | undefined {
  if (!(event.ctrlKey || event.metaKey) || event.altKey) return undefined
  switch (event.key) {
    case '=': case '+': return 'in'
    case '-': case '_': return 'out'
    case '0': return 'reset'
    default: return undefined
  }
}

/**
 * @param stage The scrolling element whose Ctrl+wheel zooms; the keys work anywhere in the page.
 * @param keys Zoom actions; the latest ones are used, so they need not be stable.
 * @param enabled Whether the viewer can zoom now.
 */
export function useZoomKeys(stage: HTMLElement | null, keys: ZoomKeys, enabled = true): void {
  const latest = useRef(keys)
  latest.current = keys
  useEffect(() => {
    if (!enabled) return
    const onKey = (event: KeyboardEvent): void => {
      const zoom = zoomKey(event)
      if (zoom === undefined || event.defaultPrevented) return
      event.preventDefault()
      if (zoom === 'in') latest.current.zoomIn()
      else if (zoom === 'out') latest.current.zoomOut()
      else latest.current.reset()
    }
    // React's wheel listeners are passive, so the page would scroll as well; this one may cancel it.
    const onWheel = (event: WheelEvent): void => {
      if (!(event.ctrlKey || event.metaKey) || event.deltaY === 0) return
      event.preventDefault()
      if (event.deltaY < 0) latest.current.zoomIn(event.clientX, event.clientY)
      else latest.current.zoomOut(event.clientX, event.clientY)
    }
    window.addEventListener('keydown', onKey)
    stage?.addEventListener('wheel', onWheel, { passive: false })
    return () => {
      window.removeEventListener('keydown', onKey)
      stage?.removeEventListener('wheel', onWheel)
    }
  }, [stage, enabled])
}
