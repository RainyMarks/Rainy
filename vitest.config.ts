import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'
import { codemirrorAlias } from './scripts/codemirror-alias.ts'

export default defineConfig({
  plugins: [react()],
  resolve: { alias: codemirrorAlias() },
  test: {
    include: ['tests/**/*.test.ts', 'tests/**/*.test.tsx'],
    exclude: ['node_modules/**'],
    testTimeout: 30000,
  },
})
