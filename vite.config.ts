/** Renderer build: one React page served by the Host at `/`. */
import { realpathSync } from 'node:fs'
import { resolve } from 'node:path'
import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import { codemirrorAlias } from './scripts/codemirror-alias.ts'
import { thirdPartyNotices } from './scripts/vite-notices.ts'

/** Single installed KaTeX root shared by rehype-katex, Milkdown Crepe and Mermaid to avoid bundling 3 copies. */
const KATEX_ROOT = realpathSync(resolve(import.meta.dirname, 'node_modules/katex'))

/**
 * Common programming, markup, data and shell languages kept in CodeMirror's rich Markdown code block picker.
 * Pruning the remaining 100+ obscure legacy modes saves ~900 KiB and ~90 lazy chunks without affecting everyday code blocks.
 */
const CODEMIRROR_COMMON_LANGUAGES = new Set([
  'C', 'C++', 'C#', 'CMake', 'CSS', 'diff', 'Dockerfile', 'Go', 'HTML', 'HTTP',
  'Java', 'JavaScript', 'JSON', 'JSX', 'Kotlin', 'LaTeX', 'LESS', 'Lua', 'Markdown',
  'Nginx', 'Objective-C', 'PHP', 'PowerShell', 'Properties files', 'ProtoBuf',
  'Python', 'R', 'Ruby', 'Rust', 'SCSS', 'Shell', 'SQL', 'Swift', 'TOML', 'TSX',
  'TypeScript', 'Vue', 'XML', 'YAML',
])

/**
 * KaTeX's stylesheets list every font as WOFF2, WOFF and TrueType. Chromium always takes WOFF2, so the other two
 * (~0.7 MB) are stripped from CSS and dropped from the bundle; fails the build if upstream CSS changes silently break stripping.
 */
const katexWoff2Only: Plugin = {
  name: 'rainy-katex-woff2-only',
  generateBundle(_options, bundle) {
    const fallbacks = /,\s*url\([^)]*KaTeX_[^)]*\.woff\)\s*format\("woff"\)\s*,\s*url\([^)]*KaTeX_[^)]*\.ttf\)\s*format\("truetype"\)/gu
    const styles: string[] = []
    let sawKatexCss = false
    let strippedFallbacks = 0
    for (const item of Object.values(bundle)) {
      if (item.type !== 'asset' || !item.fileName.endsWith('.css')) continue
      const source = String(item.source)
      if (source.includes('KaTeX_')) sawKatexCss = true
      const updated = source.replace(fallbacks, () => { strippedFallbacks++; return '' })
      item.source = updated
      styles.push(updated)
    }
    for (const [name, item] of Object.entries(bundle)) {
      const file = item.fileName.split('/').at(-1)!
      if (/^KaTeX_.*\.(?:woff|ttf)$/u.test(file) && !styles.some(style => style.includes(file))) delete bundle[name]
    }
    const remainingLegacy = Object.values(bundle).filter(item => /^KaTeX_.*\.(?:woff|ttf)$/u.test(item.fileName.split('/').at(-1)!))
    if (sawKatexCss && (strippedFallbacks === 0 || remainingLegacy.length > 0)) {
      this.error(`rainy-katex-woff2-only did not strip legacy KaTeX fonts cleanly (stripped=${strippedFallbacks}, remaining=${remainingLegacy.length})`)
    }
  },
}

/**
 * Filter `@codemirror/language-data` down to {@link CODEMIRROR_COMMON_LANGUAGES}.
 * Validates that every requested language matched so an upstream format change fails loudly instead of silently regressing.
 */
const codemirrorCommonLanguages: Plugin = {
  name: 'rainy-codemirror-common-languages',
  transform(code, id) {
    if (!/@codemirror[\\/]language-data[\\/]dist[\\/]index\.js$/u.test(id)) return null
    const startMarker = 'const languages = ['
    const endMarker = '\n];'
    const start = code.indexOf(startMarker)
    const end = code.indexOf(endMarker, start)
    if (start < 0 || end < 0) this.error('rainy-codemirror-common-languages: unexpected @codemirror/language-data structure')
    const body = code.slice(start + startMarker.length, end)
    const entryPattern = /\/\*\s*@__PURE__\s*\*\/LanguageDescription\.of\(\{[\s\S]*?\}\)(?=,?\s*(?:\/\*\s*@__PURE__\s*\*\/LanguageDescription\.of|\/\/|$))/gu
    const kept: string[] = []
    const matched = new Set<string>()
    for (const match of body.matchAll(entryPattern)) {
      const block = match[0]
      const name = /\bname:\s*"([^"]+)"/u.exec(block)?.[1]
      if (name !== undefined && CODEMIRROR_COMMON_LANGUAGES.has(name)) {
        kept.push(`    ${block}`)
        matched.add(name)
      }
    }
    if (matched.size !== CODEMIRROR_COMMON_LANGUAGES.size) {
      const missing = [...CODEMIRROR_COMMON_LANGUAGES].filter(name => !matched.has(name))
      this.error(`rainy-codemirror-common-languages: missing expected languages (${missing.join(', ')})`)
    }
    return {
      code: `${code.slice(0, start + startMarker.length)}\n${kept.join(',\n')}\n${code.slice(end)}`,
      map: null,
    }
  },
}

/**
 * Mermaid bundles optional ELK layout (~1.5 MB) and experimental cynefin-beta diagram (~638 KiB).
 * Leaving both out preserves every standard diagram (flowchart, sequence, class, state, er, gantt, pie, mindmap,
 * timeline, gitGraph, journey, etc.) while verifying during build that neither chunk sneaks back in.
 */
const mermaidTrim: Plugin = {
  name: 'rainy-mermaid-trim',
  transform(code, id) {
    if (/mermaid[\\/]dist[\\/]chunks[\\/]mermaid\.core[\\/]/u.test(id) && code.includes('...elkLayoutLoaders()')) {
      return { code: code.replace('...elkLayoutLoaders()', ''), map: null }
    }
    if (/mermaid[\\/]dist[\\/]mermaid\.core\.mjs$/u.test(id)) {
      const cynefinImport = /await import\("\.\/chunks\/mermaid\.core\/cynefinDiagram-[^"]+\.mjs"\)/u
      if (!cynefinImport.test(code)) this.error('rainy-mermaid-trim: cynefin loader pattern no longer matches mermaid.core.mjs')
      return {
        code: code.replace(cynefinImport, 'await Promise.reject(new Error("cynefin diagram is not bundled"))'),
        map: null,
      }
    }
    if (/@mermaid-js[\\/]parser[\\/]dist[\\/]mermaid-parser\.core\.mjs$/u.test(id)) {
      const cynefinParserImport = /await import\("\.\/chunks\/mermaid-parser\.core\/cynefin-[^"]+\.mjs"\)/u
      if (!cynefinParserImport.test(code)) this.error('rainy-mermaid-trim: cynefin parser pattern no longer matches mermaid-parser.core.mjs')
      return {
        code: code.replace(cynefinParserImport, 'await Promise.reject(new Error("cynefin parser is not bundled"))'),
        map: null,
      }
    }
    return null
  },
  generateBundle(_options, bundle) {
    const names = Object.keys(bundle)
    if (names.some(name => /cynefin|elk-api|elk\.bundled/iu.test(name))) {
      this.error('rainy-mermaid-trim: excluded Mermaid ELK or cynefin chunk was still emitted')
    }
    const coreDiagrams = ['flowDiagram', 'sequenceDiagram', 'classDiagram', 'stateDiagram', 'erDiagram']
    if (names.some(name => /mermaid\.core/u.test(name))) {
      const missing = coreDiagrams.filter(diagram => !names.some(name => name.includes(diagram)))
      if (missing.length > 0) this.error(`rainy-mermaid-trim: expected core Mermaid chunks missing (${missing.join(', ')})`)
    }
  },
}

/**
 * Emit `sheet-worker.ts` inside the main Rollup graph (instead of Vite's isolated worker sub-build) so the
 * Web Worker and main-thread fallback share a single `xlsx` ESM chunk (~488 KiB saved).
 */
const sharedSheetWorker: Plugin = {
  name: 'rainy-shared-sheet-worker',
  apply: 'build',
  buildStart() {
    this.emitFile({
      type: 'chunk',
      id: resolve(import.meta.dirname, 'src/renderer/ide/viewers/sheet-worker.ts'),
      name: 'sheet-worker',
    })
  },
  generateBundle(_options, bundle) {
    const workerChunk = Object.values(bundle).find(item => item.type === 'chunk' && item.name === 'sheet-worker')
    if (workerChunk === undefined) this.error('rainy-shared-sheet-worker: sheet-worker chunk was not emitted')
    const urlLiteral = JSON.stringify(`/${workerChunk.fileName}`)
    let replaced = 0
    for (const item of Object.values(bundle)) {
      if (item.type !== 'chunk' || !item.code.includes('__RAINY_SHEET_WORKER_URL__')) continue
      item.code = item.code.replaceAll('__RAINY_SHEET_WORKER_URL__', urlLiteral)
      replaced++
    }
    if (replaced === 0) this.error('rainy-shared-sheet-worker: placeholder __RAINY_SHEET_WORKER_URL__ was not found in output chunks')
    const heavyXlsx = Object.values(bundle).filter(
      item => item.type === 'chunk' && /xlsx|sheet-worker/iu.test(item.fileName) && Buffer.byteLength(item.code) > 100 * 1024,
    )
    if (heavyXlsx.length > 1) {
      this.error(`rainy-shared-sheet-worker: duplicate heavy xlsx bundles detected (${heavyXlsx.map(c => c.fileName).join(', ')})`)
    }
  },
}

export default defineConfig({
  root: resolve(import.meta.dirname, 'src/renderer'),
  base: '/',
  plugins: [
    react(),
    katexWoff2Only,
    codemirrorCommonLanguages,
    mermaidTrim,
    sharedSheetWorker,
    thirdPartyNotices('RainyAgent interface: third-party license and notice texts.'),
  ],
  resolve: {
    alias: {
      ...codemirrorAlias(),
      katex: KATEX_ROOT,
    },
  },
  worker: {
    format: 'es',
  },
  build: {
    outDir: resolve(import.meta.dirname, 'dist/renderer'),
    emptyOutDir: true,
    target: 'chrome130',
    sourcemap: false,
    chunkSizeWarningLimit: 4096,
  },
})
