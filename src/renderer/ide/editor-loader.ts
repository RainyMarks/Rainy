/** Load the editor module once; all callers share the same result. */
import type { EditorAssets } from './editor-types.ts'

let pending: Promise<EditorAssets> | undefined

/**
 * Load the CodeMirror editor and xterm terminal on first use; the bundler splits them into their own chunk.
 * @returns The editor and terminal factories.
 */
export function loadEditorAssets(): Promise<EditorAssets> {
  pending ??= import('../../editor/editor.ts').then(module => module.assets).catch((error: unknown) => {
    pending = undefined
    throw error
  })
  return pending
}
