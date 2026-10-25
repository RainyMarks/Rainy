/** Mermaid diagrams in Markdown (```mermaid fences), drawn as SVG. The library loads with the first diagram, outside the
 *  main bundle, and renders with its strict security level: no scripts, no clicks, sanitized labels. */
import { isDark } from '../../prefs.ts'

type Mermaid = typeof import('mermaid').default

let library: Promise<Mermaid> | undefined
let queue: Promise<unknown> = Promise.resolve()
let counter = 0

/** @param language Fence language. @returns Whether the fence holds a Mermaid diagram. */
export function isMermaid(language: string | undefined): boolean {
  return language?.trim().toLowerCase() === 'mermaid'
}

/**
 * Draw one diagram; diagrams render one at a time, since Mermaid measures text in a shared hidden element.
 * @param source Diagram text.
 * @param dark Whether to use the dark theme; defaults to the page's.
 * @returns SVG markup.
 */
export function renderMermaid(source: string, dark = isDark()): Promise<string> {
  const task = queue.then(async () => {
    library ??= import('mermaid').then(module => module.default, (error: unknown) => { library = undefined; throw error })
    const mermaid = await library
    const style = getComputedStyle(document.body)
    const token = (name: string): string => style.getPropertyValue(name).trim()
    const fontFamily = token('--rainy-font-family') || undefined
    // In the page's own mode the diagram takes the page's colors: Mermaid's dark theme drew near-black boxes on Rainy's
    // blue-gray pages. A diagram for the other mode (an export) keeps Mermaid's matching theme.
    const block = dark ? token('--rainy-alias-bg-layer-3') : token('--rainy-static-gray-50')
    const themed = dark === isDark() && block !== ''
    mermaid.initialize(themed ? {
      startOnLoad: false, securityLevel: 'strict', theme: 'base', fontFamily, darkMode: dark,
      themeVariables: {
        background: token('--rainy-alias-bg-base'), primaryColor: block, secondaryColor: block, tertiaryColor: block,
        primaryTextColor: token('--rainy-alias-label-primary'), textColor: token('--rainy-alias-label-primary'),
        primaryBorderColor: token('--rainy-alias-state-business-primary'), lineColor: token('--rainy-alias-label-secondary'),
        fontSize: '14px',
      },
    } : { startOnLoad: false, securityLevel: 'strict', theme: dark ? 'dark' : 'default', fontFamily })
    const { svg } = await mermaid.render(`rainy-mermaid-${++counter}`, source)
    return svg
  })
  queue = task.catch(() => undefined)
  return task
}

const themeListeners = new Set<() => void>()
let themeObserver: MutationObserver | undefined
let shownTheme = ''

/** What a diagram's colors and text measurement depend on: the mode, the color scheme, the accent and the font. */
function themeKey(): string {
  const body = document.body
  return [isDark(), body.getAttribute('data-color-scheme'), body.getAttribute('data-accent'),
    getComputedStyle(body).getPropertyValue('--rainy-font-family')].join('|')
}

/**
 * Follow the page's theme, so drawn diagrams can be drawn again in the new colors.
 * @param listener Called after the dark mode, color scheme, accent or interface font changed.
 * @returns Unsubscribe.
 */
export function onThemeChange(listener: () => void): () => void {
  themeListeners.add(listener)
  if (themeObserver === undefined && typeof MutationObserver !== 'undefined' && typeof document !== 'undefined') {
    shownTheme = themeKey()
    // `prefs.ts` sets these attributes on every preference change; only a change of their values counts.
    themeObserver = new MutationObserver(() => {
      const key = themeKey()
      if (key === shownTheme) return
      shownTheme = key
      for (const notify of [...themeListeners]) notify()
    })
    themeObserver.observe(document.body, { attributes: true, attributeFilter: ['data-rainy-dark', 'data-color-scheme', 'data-accent'] })
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['style'] })
  }
  return () => {
    themeListeners.delete(listener)
    if (themeListeners.size > 0) return
    themeObserver?.disconnect()
    themeObserver = undefined
  }
}
