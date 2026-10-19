/** Renderer build: one React page served by the Host at `/`. */
import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'
import { thirdPartyNotices } from './scripts/vite-notices.ts'

/**
 * KaTeX's stylesheets (chat math and the WYSIWYG Markdown editor each bring one) list every font as WOFF2, WOFF and
 * TrueType. Chromium always takes WOFF2, so the other two (about 0.7 MB) would only take up room in the installer and
 * the Linux runtime: the stylesheets keep the WOFF2 source, and the files no stylesheet names any more are dropped.
 */
const katexWoff2Only: Plugin = {
  name: 'rainy-katex-woff2-only',
  generateBundle(_options, bundle) {
    const fallbacks = /,\s*url\([^)]*KaTeX_[^)]*\.woff\)\s*format\("woff"\)\s*,\s*url\([^)]*KaTeX_[^)]*\.ttf\)\s*format\("truetype"\)/gu
    const styles: string[] = []
    for (const item of Object.values(bundle)) {
      if (item.type !== 'asset' || !item.fileName.endsWith('.css')) continue
      item.source = String(item.source).replace(fallbacks, '')
      styles.push(item.source)
    }
    for (const [name, item] of Object.entries(bundle)) {
      const file = item.fileName.split('/').at(-1)!
      if (/^KaTeX_.*\.(?:woff|ttf)$/u.test(file) && !styles.some(style => style.includes(file))) delete bundle[name]
    }
  },
}

export default defineConfig({
  root: resolve(import.meta.dirname, 'src/renderer'),
  base: '/',
  plugins: [react(), katexWoff2Only, thirdPartyNotices('RainyAgent interface: third-party license and notice texts.')],
  build: {
    outDir: resolve(import.meta.dirname, 'dist/renderer'),
    emptyOutDir: true,
    target: 'chrome130',
    sourcemap: false,
    chunkSizeWarningLimit: 4096,
  },
})
