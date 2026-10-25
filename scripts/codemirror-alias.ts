/** The CodeMirror packages the rich Markdown editor's code theme imports (src/renderer/ide/viewers/code-theme.ts) are
 *  Crepe's own dependencies, not Rainy's: resolve them where Crepe does, so the page and Crepe share one instance of
 *  each (CodeMirror rejects extensions from a second copy of @codemirror/state). */
import { realpathSync } from 'node:fs'
import { join, resolve } from 'node:path'

/** @param from A package folder. @param name A dependency of it. @returns The folder Node loads that dependency from. */
function dependency(from: string, name: string): string {
  // pnpm links a package's dependencies next to it, under the same node_modules.
  return realpathSync(join(from, ...name.split('/').map(() => '..'), name))
}

/** @returns Vite aliases from each package name to the folder Crepe loads it from. */
export function codemirrorAlias(): Record<string, string> {
  const crepe = realpathSync(resolve(import.meta.dirname, '../node_modules/@milkdown/crepe'))
  const language = dependency(crepe, '@codemirror/language')
  return {
    '@codemirror/language': language,
    '@codemirror/state': dependency(crepe, '@codemirror/state'),
    '@codemirror/view': dependency(crepe, '@codemirror/view'),
    '@lezer/highlight': dependency(language, '@lezer/highlight'),
  }
}
