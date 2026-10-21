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
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: dark ? 'dark' : 'default',
      fontFamily: getComputedStyle(document.body).getPropertyValue('--rainy-font-family').trim() || undefined })
    const { svg } = await mermaid.render(`rainy-mermaid-${++counter}`, source)
    return svg
  })
  queue = task.catch(() => undefined)
  return task
}
