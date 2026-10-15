// @vitest-environment happy-dom
/** Agent activity for the map and the file tree: a call shows from the moment the model writes it until a while after it ran. */
import { afterEach, expect, it, vi } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import type { SessionSummary } from '../../src/shared/rpc.ts'
import { getAiActivity, LINGER_MS } from '../../src/renderer/map/activity.ts'
import { emit, handle } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)

afterEach(() => { vi.useRealTimers() })

it('follows a write from the model writing it, through running it, to a moment after', async () => {
  handle('sessions.list', () => [])
  await emit('sessions.changed', { id: 's1', cwd: '/ctf/baby' } as SessionSummary)
  const partial = { role: 'assistant', content: [{ type: 'toolCall', id: 'c1', name: 'write', arguments: { file_path: 'solve.py' } }] } as unknown as AssistantMessage
  await emit('session.stream', { sessionId: 's1', message: partial })
  expect(getAiActivity().active).toMatchObject([{ toolCallId: 'c1', action: 'write', path: '/ctf/baby/solve.py' }])

  await emit('session.tool', { sessionId: 's1', toolCallId: 'c1', toolName: 'write', phase: 'start', args: { file_path: 'solve.py', content: 'x' } })
  await emit('session.tool', { sessionId: 's1', toolCallId: 'c2', toolName: 'bash', phase: 'start', args: { command: 'python solve.py', description: 'Run it' } })
  expect(getAiActivity().active.map(item => item.toolCallId)).toEqual(['c1', 'c2'])

  vi.useFakeTimers({ shouldAdvanceTime: true })
  await emit('session.tool', { sessionId: 's1', toolCallId: 'c1', toolName: 'write', phase: 'end' })
  expect(getAiActivity().recent[0]).toMatchObject({ toolCallId: 'c1', endedAt: expect.any(Number) })
  // The write stays shown for a moment after it ran; a finished run clears what was still running.
  await emit('session.state', { sessionId: 's1', status: 'idle', queue: [], model: null, context: null, permission: 'ask', approvals: [] })
  expect(getAiActivity().active.map(item => item.toolCallId)).toEqual(['c1'])
  vi.advanceTimersByTime(LINGER_MS + 1)
  expect(getAiActivity().active).toEqual([])
  expect(getAiActivity().recent.map(item => item.toolCallId)).toEqual(['c1'])
})
