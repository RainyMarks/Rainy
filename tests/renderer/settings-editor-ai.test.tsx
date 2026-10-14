// @vitest-environment happy-dom
/** Models → AI in the editor stores whether suggestions are on and which model gives them. */
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { ModelsStatus, UiPreferences } from '../../src/shared/rpc.ts'
import { EditorAiSettings } from '../../src/renderer/settings/EditorAiSettings.tsx'
import { settingsMessages } from '../../src/renderer/settings/messages.ts'
import { DEFAULT_PREFS } from '../../src/host/settings.ts'
import { button, choose, cleanup, click, emit, handle, render } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const zh = settingsMessages.zh
let stored: UiPreferences
const status = {
  models: [{ provider: 'rainy-deepseek', model: 'deepseek-v4.1-flash' }, { provider: 'local-qwen', model: 'qwen3-coder' }],
  presets: [{ name: 'DeepSeek V4.1 Flash', model: { provider: 'rainy-deepseek', model: 'deepseek-v4.1-flash' } }],
} as unknown as ModelsStatus

beforeEach(async () => {
  stored = { ...DEFAULT_PREFS }
  handle('prefs.set', (change) => { stored = { ...stored, ...change }; return stored })
  await emit('prefs.changed', stored)
})
afterEach(async () => { await cleanup() })

it('turns suggestions on and picks their model, falling back to the chat model for a removed one', async () => {
  await render(<EditorAiSettings status={status} />)
  expect(button(zh.editorAiSuggest).getAttribute('aria-checked')).toBe('false')
  await click(button(zh.editorAiSuggest))
  expect(stored.inlineCompletion).toBe(true)
  expect(button(zh.editorAiModel).textContent).toBe(zh.editorAiSameModel)
  await choose(zh.editorAiModel, 'local-qwen/qwen3-coder')
  expect(stored.completionModel).toBe('local-qwen')
  await choose(zh.editorAiModel, 'DeepSeek V4.1 Flash')
  expect(stored.completionModel).toBe('rainy-deepseek')
  await emit('prefs.changed', { ...stored, completionModel: 'gone' })
  expect(button(zh.editorAiModel).textContent).toBe(zh.editorAiSameModel)
})
