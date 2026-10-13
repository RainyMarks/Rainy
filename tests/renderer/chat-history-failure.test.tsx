// @vitest-environment happy-dom
/** A chat history the Host cannot list is reported instead of leaving the list empty. */
import { afterEach, expect, it, vi } from 'vitest'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { UiPreferences } from '../../src/shared/rpc.ts'
import { History } from '../../src/renderer/chat/History.tsx'
import { cleanup, emit, handle, render, toast, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const prefs: UiPreferences = { locale: 'zh', theme: 'system', uiFontSize: 14, codeFontSize: 13, busyEnter: 'queue', stepDetail: 'standard', showUsage: true }
afterEach(async () => { await cleanup() })

it('says the history could not be read', async () => {
  handle('prefs.get', () => prefs)
  handle('sessions.list', () => { throw new Error('index.json is unreadable') })
  await emit('prefs.changed', prefs)
  await render(<History workspaceId={'w' as WorkspaceId} currentSessionId={null} onSelect={() => undefined} onNewChat={() => undefined} />)
  await waitFor(() => { expect(toast).toHaveBeenCalledWith('无法读取对话历史：index.json is unreadable', { tone: 'error' }) })
})
