// @vitest-environment happy-dom
/** A tool card follows its call: closed while the reply streams, open with live output once the call runs. */
import { afterEach, expect, it, vi } from 'vitest'
import type { AssistantMessage, ToolCall } from '@earendil-works/pi-ai'
import type { TranscriptEntry } from '../../src/shared/rpc.ts'
import type { ChatView } from '../../src/renderer/chat/chat-store.ts'
import { Transcript } from '../../src/renderer/chat/Transcript.tsx'
import { all, cleanup, render } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

afterEach(async () => { await cleanup() })

const call: ToolCall = { type: 'toolCall', id: 'c1', name: 'bash', arguments: { command: 'sleep 5; echo hi', description: 'Wait, then greet' } }
const reply = {
  role: 'assistant', content: [call], stopReason: 'toolUse', timestamp: 1, api: 'openai-completions', provider: 'p', model: 'm',
  usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
} satisfies AssistantMessage
const user: TranscriptEntry = { id: 'u', kind: 'user', ts: 1, text: 'go' }
const answer: TranscriptEntry = { id: 'a', kind: 'assistant', ts: 1, message: reply }

function view(entries: TranscriptEntry[], tools: ChatView['tools'], streaming: AssistantMessage | null = null): ChatView {
  return {
    summary: { id: 's', workspaceId: null, cwd: '/p', title: 't', createdAt: 1, updatedAt: 1, archived: false, pinned: false, model: null, status: 'running' },
    entries, streaming, tools, truncated: false, loading: false, queue: [], model: null, context: null, runningTools: Object.keys(tools),
    permission: 'ask', approvals: [], background: [],
  } as ChatView
}

it('opens a card when its call starts running after the reply arrived', async () => {
  const shown = await render(<Transcript view={view([user], {}, reply)} onFork={() => undefined} />)
  const expanded = (): (string | null)[] => all('[data-tool] button[aria-expanded]', shown.container).map(button => button.getAttribute('aria-expanded'))
  expect(expanded()).toEqual(['false'])
  // The Host sends the reply's entry first and the call's start right after.
  await shown.rerender(<Transcript view={view([user, answer], {})} onFork={() => undefined} />)
  await shown.rerender(<Transcript view={view([user, answer], { c1: { toolName: 'bash', partial: 'live output' } })} onFork={() => undefined} />)
  expect(expanded()).toEqual(['true'])
  expect(shown.container.textContent).toContain('live output')
})
