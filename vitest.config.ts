import { defineConfig } from 'vitest/config'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  test: {
    include: ['tests/**/*.test.ts', 'tests/**/*.test.tsx'],
    exclude: ['tests/legacy/**', 'tests/legacy-ui/**', 'node_modules/**'],
    testTimeout: 30000,
  },
})
