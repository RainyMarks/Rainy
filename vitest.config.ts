import { realpathSync } from 'node:fs'
import { resolve } from 'node:path'
import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import { codemirrorAlias } from './scripts/codemirror-alias.ts'

const KATEX_ROOT = realpathSync(resolve(import.meta.dirname, 'node_modules/katex'))

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      ...codemirrorAlias(),
      katex: KATEX_ROOT,
    },
  },
  test: {
    include: ['tests/**/*.test.ts', 'tests/**/*.test.tsx'],
    exclude: ['node_modules/**'],
    setupFiles: ['./tests/renderer/standards-mode.ts'],
    testTimeout: 30000,
  },
})
