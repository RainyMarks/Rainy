/** Loads the KaTeX rehype plugin on demand and re-renders Markdown that waited for it. */
import { useSyncExternalStore } from 'react'

type RehypeKatex = typeof import('rehype-katex').default

let plugin: RehypeKatex | undefined
let loading: Promise<void> | undefined
const listeners = new Set<() => void>()

/**
 * Whether text may contain math for remark-math: `$…$` or `$$…$$`.
 * @param text Markdown source.
 * @returns Whether KaTeX is needed to render it.
 */
export function mayContainMath(text: string): boolean {
  return text.includes('$')
}

function load(): void {
  loading ??= import('./katex-plugin.ts').then((module) => {
    plugin = module.rehypeKatex
    for (const listener of listeners) listener()
  }, (error: unknown) => {
    loading = undefined
    console.error('KaTeX failed to load', error)
  })
}

/**
 * @param text Markdown source.
 * @returns The KaTeX plugin once loaded; starts loading it when the text needs it.
 */
export function useMathPlugin(text: string): RehypeKatex | undefined {
  const current = useSyncExternalStore(
    (listener) => { listeners.add(listener); return () => { listeners.delete(listener) } },
    () => plugin,
  )
  if (current === undefined && mayContainMath(text)) load()
  return current
}
