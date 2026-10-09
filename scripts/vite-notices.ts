/** Vite plugin that writes the license and notice texts of every npm package bundled into the renderer. */
import { readdir, readFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import type { Plugin } from 'vite'

interface PackageSource { name: string; version: string; license: string | undefined; author: string | undefined; directory: string }

const LICENSE_FILE = /^(licen[cs]e|copying|notice|third[-_ ]?party[-_ ]?notices)/i

/** MIT terms for packages whose license file lives only in their monorepo root, with the author `package.json` names. */
function mitText(author: string): string {
  return `MIT License\n\nCopyright (c) ${author}\n\nPermission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:\n\nThe above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.\n\nTHE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.\n`
}

async function packageOf(file: string, cache: Map<string, Promise<PackageSource | undefined>>): Promise<PackageSource | undefined> {
  const directory = dirname(file)
  let pending = cache.get(directory)
  if (pending === undefined) {
    pending = (async () => {
      try {
        const metadata = JSON.parse(await readFile(join(directory, 'package.json'), 'utf8')) as { name?: unknown; version?: unknown; license?: unknown; author?: unknown }
        if (typeof metadata.name === 'string' && typeof metadata.version === 'string') {
          const author = typeof metadata.author === 'string' ? metadata.author
            : typeof metadata.author === 'object' && metadata.author !== null && 'name' in metadata.author && typeof metadata.author.name === 'string' ? metadata.author.name : undefined
          return { name: metadata.name, version: metadata.version, license: typeof metadata.license === 'string' ? metadata.license : undefined, author, directory }
        }
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error
      }
      return dirname(directory) === directory ? undefined : packageOf(directory, cache)
    })()
    cache.set(directory, pending)
  }
  return pending
}

/**
 * Emit `THIRD_PARTY_NOTICES.txt` next to the renderer bundle.
 * @param title First line of the notices file.
 * @returns The plugin; the build fails when a bundled package ships no license file and is not MIT with a named author.
 */
export function thirdPartyNotices(title: string): Plugin {
  return {
    name: 'rainy-third-party-notices',
    apply: 'build',
    async generateBundle() {
      const cache = new Map<string, Promise<PackageSource | undefined>>()
      const packages = new Map<string, PackageSource>()
      for (const id of this.getModuleIds()) {
        // Rollup marks virtual modules with a leading NUL; they have no package of their own.
        if (id.startsWith('\0')) continue
        const file = id.replace(/\?.*$/, '')
        if (!file.includes('node_modules')) continue
        const source = await packageOf(file, cache)
        if (source !== undefined) packages.set(`${source.name}@${source.version}`, source)
      }
      const sections = [`${title}\n`]
      const missing: string[] = []
      for (const [id, source] of [...packages].sort(([left], [right]) => left.localeCompare(right))) {
        const files = (await readdir(source.directory, { withFileTypes: true }))
          .filter(entry => entry.isFile() && LICENSE_FILE.test(entry.name)).map(entry => entry.name).sort()
        sections.push(`\n${'='.repeat(72)}\n${id}\nLicense: ${source.license ?? 'See notice below'}\n`)
        if (files.length === 0) {
          if (source.license === 'MIT' && source.author !== undefined) sections.push(`\n${mitText(source.author)}`)
          else missing.push(id)
          continue
        }
        for (const name of files) sections.push(`\n--- ${name} ---\n${await readFile(join(source.directory, name), 'utf8')}\n`)
      }
      if (missing.length > 0) this.error(`Bundled packages without a license file: ${missing.join(', ')}`)
      this.emitFile({ type: 'asset', fileName: 'THIRD_PARTY_NOTICES.txt', source: sections.join('') })
    },
  }
}
