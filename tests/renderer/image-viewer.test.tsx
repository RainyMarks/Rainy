// @vitest-environment happy-dom
/** The image viewer's controls are named in the interface language. */
import { afterEach, expect, it, vi } from 'vitest'
import { ImageViewer } from '../../src/renderer/ide/viewers/ImageViewer.tsx'
import { cleanup, emit, render } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
afterEach(async () => { await cleanup() })

it('names the zoom buttons instead of reading out their glyphs', async () => {
  await emit('prefs.changed', { locale: 'en', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true, colorScheme: 'graphite', accent: 'auto', uiFont: '', codeFont: '', inlineCompletion: false, completionModel: '', autoKeepChanges: true, rainEffects: true, webSearch: true, widgets: true })
  await render(<ImageViewer url="/rainy/ide/raw?path=a.png" name="a.png" bytes={10} />)
  const labels = [...document.querySelectorAll('button')].map(button => button.getAttribute('aria-label')).filter(label => label !== null)
  expect(labels).toEqual(expect.arrayContaining(['Zoom in', 'Zoom out']))
  expect(labels).not.toContain('−')
  expect(document.querySelector('select')).toBeNull()
})
