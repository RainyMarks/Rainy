/** Page theme and viewport size as React values. */
import { useEffect, useState } from 'react'
import { isDark } from '../prefs.ts'

/** @returns Whether the page uses the dark palette; follows `body[data-ds-dark-theme]`, including system changes. */
export function useDarkTheme(): boolean {
  const [dark, setDark] = useState(isDark)
  useEffect(() => {
    const observer = new MutationObserver(() => { setDark(isDark()) })
    observer.observe(document.body, { attributes: true, attributeFilter: ['data-ds-dark-theme'] })
    setDark(isDark())
    return () => { observer.disconnect() }
  }, [])
  return dark
}

/** @returns The window's inner size; re-renders on resize. */
export function useViewportSize(): { readonly width: number; readonly height: number } {
  const [size, setSize] = useState(() => ({ width: window.innerWidth, height: window.innerHeight }))
  useEffect(() => {
    const resize = (): void => { setSize({ width: window.innerWidth, height: window.innerHeight }) }
    window.addEventListener('resize', resize)
    return () => { window.removeEventListener('resize', resize) }
  }, [])
  return size
}
