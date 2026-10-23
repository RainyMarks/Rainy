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
