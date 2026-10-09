/** Renderer build: one React page served by the Host at `/`. */
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { resolve } from 'node:path'
import { thirdPartyNotices } from './scripts/vite-notices.ts'

export default defineConfig({
  root: resolve(import.meta.dirname, 'src/renderer'),
  base: '/',
  plugins: [react(), thirdPartyNotices('RainyAgent interface: third-party license and notice texts.')],
  build: {
    outDir: resolve(import.meta.dirname, 'dist/renderer'),
    emptyOutDir: true,
    target: 'chrome130',
    sourcemap: false,
    chunkSizeWarningLimit: 4096,
  },
})
