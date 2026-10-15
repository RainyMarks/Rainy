/** Errors from the desktop process reach Settings without Electron's IPC prefix. */
import { expect, it } from 'vitest'
import { errorText } from '../../src/renderer/settings/parts.tsx'

it('keeps only the reason the desktop process wrote', () => {
  expect(errorText(new Error("Error invoking remote method 'rainy:runtime-ssh-add': Error: 找不到主机 dev。"))).toBe('找不到主机 dev。')
  expect(errorText(new Error('Plain failure'))).toBe('Plain failure')
  expect(errorText('text')).toBe('text')
})
