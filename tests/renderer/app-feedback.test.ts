// @vitest-environment happy-dom
/** Copy controls that only show success, and requests lost with the connection, still tell the user what failed. */
import { afterEach, expect, it, vi } from 'vitest'
import type { UiPreferences } from '../../src/shared/rpc.ts'
import { emit } from './settings-harness.tsx'

const disconnection = vi.hoisted(() => ({ text: undefined as (() => string) | undefined }))
vi.mock('../../src/renderer/rpc.ts', async () => ({
  ...(await import('./settings-harness.tsx')).rpcModule,
  describeDisconnection: (text: () => string) => { disconnection.text = text },
}))
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const prefs = (locale: 'zh' | 'en'): UiPreferences => ({ locale, theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true })

afterEach(() => { vi.unstubAllGlobals() })

it('announces a refused copy in the interface language unless the control reports it itself', async () => {
  const { toast } = await import('./settings-harness.tsx')
  const { installFeedback } = await import('../../src/renderer/app/feedback.ts')
  const { writeClipboard } = await import('../../src/renderer/ui/clipboard.ts')
  vi.stubGlobal('navigator', { clipboard: { writeText: vi.fn(async () => { throw new DOMException('Document is not focused.', 'NotAllowedError') }) } })
  await emit('prefs.changed', prefs('zh'))
  const remove = installFeedback()
  expect(await writeClipboard('flag{x}')).toBe(false)
  expect(toast).toHaveBeenLastCalledWith('复制失败：系统剪贴板拒绝了写入，请点击窗口后重试。', { tone: 'error' })
  toast.mockClear()
  expect(await writeClipboard('flag{x}', { quiet: true })).toBe(false)
  expect(toast).not.toHaveBeenCalled()
  await emit('prefs.changed', prefs('en'))
  expect(disconnection.text?.()).toBe('The connection to Rainy was lost, so this action did not finish.')
  await writeClipboard('flag{x}')
  expect(toast).toHaveBeenLastCalledWith('Copy failed: the system clipboard refused the text. Click the window and try again.', { tone: 'error' })
  remove()
  toast.mockClear()
  await writeClipboard('flag{x}')
  expect(toast).not.toHaveBeenCalled()
})
