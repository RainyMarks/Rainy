/** Load the offline ESM editor once; all callers share the same initialization result. */
import type { EditorAssets } from './editor-types.ts'

let pending: Promise<EditorAssets> | undefined

/** Load the separately built Monaco bundle from `/rainy/editor/` on first use.
 * @returns The trusted adapter installed by the editor entry point.
 */
export function loadEditorAssets(): Promise<EditorAssets> {
  if (window.__RAINY_EDITOR_ASSETS__ !== undefined) return Promise.resolve(window.__RAINY_EDITOR_ASSETS__)
  pending ??= new Promise<EditorAssets>((resolve, reject) => {
    const stylesheet = document.createElement('link')
    stylesheet.rel = 'stylesheet'
    stylesheet.href = '/rainy/editor/editor.css'
    document.head.append(stylesheet)
    const script = document.createElement('script')
    script.type = 'module'
    script.src = '/rainy/editor/editor.js'
    script.addEventListener(
      'load',
      () => {
        const adapter = window.__RAINY_EDITOR_ASSETS__
        if (adapter?.version === 1) resolve(adapter)
        else reject(new Error('The editor adapter did not initialize.'))
      },
      { once: true },
    )
    script.addEventListener(
      'error',
      () => {
        reject(new Error('The offline editor assets could not be loaded.'))
      },
      { once: true },
    )
    document.head.append(script)
  }).catch((error: unknown) => {
    pending = undefined
    throw error
  })
  return pending
}
