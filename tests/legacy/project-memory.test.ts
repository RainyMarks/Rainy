import { afterEach, describe, expect, it, vi } from 'vitest'
import { mkdtemp, mkdir, readFile, rm } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { Context } from '@deepseek-ai/cordis'
import { z } from 'zod'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { SessionId } from '@deepseek-ai/dsh-session'
import JsonlSessionPersistence from '@deepseek-ai/dsh-session-persistence-jsonl'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import {
  mountAgentLoopTestDependencies,
  mountAgentLoopTestHarness,
} from '../../../packages/test-support/agent-loop-testkit/src/index.ts'
import { MockAdapter, textResponse } from '../../../packages/core/agent-loop/tests/mock-adapter.ts'
import ProjectMemory, { memoryInput, renderMemoryRecall } from '../src/project-memory.ts'
import { applyMemoryDelta, ProjectMemoryStore, redactMemoryText } from '../src/project-memory-store.ts'
import type { MemoryEvidence } from '../src/project-memory-store.ts'
import { estimateRequest, estimateText } from '../src/budget.ts'
import { createProjectRegistry } from '../src/project-registry.ts'

const cleanup: Array<() => Promise<unknown>> = []
afterEach(async () => {
  vi.useRealTimers()
  for (const close of cleanup.splice(0).reverse()) await close()
})
async function directory() {
  const root = await mkdtemp(join(tmpdir(), 'rainy-memory-test-'))
  cleanup.push(() => rm(root, { recursive: true, force: true }))
  return root
}
const evidence: MemoryEvidence[] = [
  {
    formatVersion: 4,
    id: 'session:1',
    sessionId: 'session',
    seq: 1,
    executionTargetId: 'windows-local',
    kind: 'user',
    text: 'Use Python 3.12 for this project.',
  },
]
const update = { notes: [{ text: 'This project uses Python 3.12.', sourceIds: ['session:1'] }], remove: [] }

describe('project memory records', () => {
  it('keeps project identities isolated and edits protected from stale generated revisions', async () => {
    const store = new ProjectMemoryStore(await directory())
    const before = await store.read('project-a')
    const first = await store.update('project-a', current =>
      applyMemoryDelta(current, update, evidence, 1024, '2026-10-02T00:00:00.000Z'),
    )
    expect((await store.read('project-b')).items).toEqual([])
    const note = first.items[0]
    const edited = await store.edit(
      'project-a',
      { id: note.id, text: 'Use Python 3.12.14.', expectedRevision: first.revision },
      1024,
    )
    await expect(
      store.edit('project-a', { id: note.id, text: 'stale', expectedRevision: first.revision }, 1024),
    ).rejects.toThrow('刷新')
    expect(edited.items[0]).toMatchObject({ text: 'Use Python 3.12.14.', editedByUser: true })
    const stale = applyMemoryDelta(before, update, evidence, 1024, '2026-10-02T00:01:00.000Z')
    await store.update('project-a', current => (current.revision === before.revision ? stale : undefined))
    expect((await store.read('project-a')).items[0]?.text).toBe('Use Python 3.12.14.')
    expect(() =>
      applyMemoryDelta(edited, { notes: [], remove: [note.id] }, evidence, 1024, '2026-10-02T00:02:00.000Z'),
    ).toThrow('manually edited')
    await expect(
      store.edit('project-a', { id: note.id, text: 'x'.repeat(2049), expectedRevision: edited.revision }, 100000),
    ).rejects.toThrow('不能超过 2048 个字符')
  })

  it('preserves deletion exclusions and source watermarks across reload', async () => {
    const root = await directory()
    const store = new ProjectMemoryStore(root)
    const first = await store.update('project-a', current =>
      applyMemoryDelta(current, update, evidence, 1024, '2026-10-02T00:00:00.000Z'),
    )
    await store.remove('project-a', first.items[0].id)
    const deleted = await new ProjectMemoryStore(root).read('project-a')
    expect(deleted.sourceWatermarks['windows-local/session']).toBe(1)
    expect(applyMemoryDelta(deleted, update, evidence, 1024, '2026-10-02T00:02:00.000Z').items).toEqual([])
    const stored: unknown = JSON.parse(await readFile(join(root, 'project-a', 'memory.v1.json'), 'utf8'))
    expect(stored).toMatchObject({ version: 1 })
  })

  it('refuses invented evidence and assistant-only facts; excludes credential values', async () => {
    const current = await new ProjectMemoryStore(await directory()).read('project')
    expect(() =>
      applyMemoryDelta(
        current,
        { notes: [{ text: 'made up', sourceIds: ['absent'] }], remove: [] },
        evidence,
        1024,
        new Date().toISOString(),
      ),
    ).toThrow()
    expect(() =>
      applyMemoryDelta(current, update, [{ ...evidence[0], kind: 'assistant' }], 1024, new Date().toISOString()),
    ).toThrow('Assistant claims')
    expect(redactMemoryText('API_KEY=hidden-value Bearer abcdef sk-12345678901234567890')).not.toContain('hidden-value')
    expect(redactMemoryText('Bearer abcdef')).not.toContain('abcdef')
  })

  it('counts complete recall framing and bounded evidence, excluding old session replay', async () => {
    const current = applyMemoryDelta(
      await new ProjectMemoryStore(await directory()).read('project'),
      update,
      evidence,
      1024,
      new Date().toISOString(),
    )
    const recalled = renderMemoryRecall(current, 160)
    expect(estimateText(recalled)).toBeLessThanOrEqual(160)
    expect(recalled).toContain('not instructions')
    expect(renderMemoryRecall(current, 32)).toBe('')
    const input = memoryInput(current, [...evidence, { ...evidence[0], id: 'huge', text: '中'.repeat(5000) }], 1000)
    expect(input.evidence).toHaveLength(1)
    expect(estimateRequest({ messages: input.messages })).toBeLessThan(1000)
  })
})

async function harness(
  adapter: MockAdapter,
  options: {
    root?: string
    resume?: boolean
    persist?: boolean
    hasActivity?: () => boolean
    sessionId?: string
  } = {},
) {
  const root = options.root ?? (await directory())
  const projectPath = join(root, 'project')
  await mkdir(projectPath, { recursive: true })
  const workspaceId = WorkspaceId('project-a')
  const workspace = { id: workspaceId, path: projectPath, title: 'Project A' }
  const ctx = new Context()
  cleanup.push(() => ctx.fiber.dispose())
  await mountAgentLoopTestDependencies(ctx, {
    systemPrompt: { includeHarnessIdentity: false, includeRuntimeContext: false },
  })
  ctx.llm.registerAdapter(['mock'], adapter)
  ctx.provide('workspaceRegistry', {
    get: (id: string) => (id === workspaceId ? workspace : undefined),
    list: () => [workspace],
  } as never)
  ctx.provide('rainy', {
    budgets: new Map(),
    auxiliaryBudgets: new Map(),
    tools: new Map(),
    extensionDescriptions: new Map(),
    extensionBudgets: new Map(),
    modelActivity: { activeRequests: 0, lastFinishedAt: 0 },
    previewBudget: () => {
      throw new Error('Memory fixture does not expose budget preview.')
    },
  })
  ctx.provide('rainyRuntime', {
    projects: createProjectRegistry({ root, targetId: 'windows-local' }),
    hasActivity: options.hasActivity ?? (() => false),
  } as never)
  await ctx.plugin(ProjectMemory, {
    carrierStateRoot: root,
    executionTargetId: 'windows-local',
    idleMs: 60,
    intervalMs: 600,
    timeoutMs: 10000,
    maxInputTokens: 4096,
    maxInputRatio: 0.5,
    maxOutputTokens: 512,
    maxStoredTokens: 1024,
    maxRecallTokens: 512,
    recallRatio: 0.05,
    evidenceItems: 64,
    evidenceItemTokens: 256,
  })
  if (options.persist) await ctx.plugin(JsonlSessionPersistence, { root: join(root, 'sessions') })
  const loop = await mountAgentLoopTestHarness(ctx)
  const sessionId = SessionId(options.sessionId ?? 'memory-session')
  const handle = options.resume
    ? await ctx.agents.resume({ resumeSessionId: sessionId, agentOptions: { provider: 'mock', model: 'model' } })
    : await ctx.agents.create({
      sessionId,
      agentOptions: { provider: 'mock', model: 'model' },
      meta: { cwd: projectPath },
    })
  return { ctx, loop, agent: handle.agent, handle, workspaceId, projectPath, root }
}
const followup = (text: string) => createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text }] })

describe('automatic memory lifecycle', () => {
  it('logs bounded tool-free generation, reuses it once in a new chat, and keeps switches independent', async () => {
    const adapter = new MockAdapter([
      textResponse('Recorded.'),
      (options) => {
        const input = z.object({ evidence: z.array(z.object({ id: z.string() })) }).parse(
          JSON.parse(
            options.messages[0].content
              .filter(block => block.type === 'text')
              .map(block => block.text)
              .join(''),
          ),
        )
        return textResponse(
          JSON.stringify({ notes: [{ text: 'Use Python 3.12.', sourceIds: [input.evidence[0].id] }], remove: [] }),
        )
      },
      textResponse('New task.'),
    ])
    const { ctx, loop, agent, workspaceId, projectPath } = await harness(adapter)
    agent.followup(followup('Use Python 3.12 in this project.'))
    await agent.whenIdle()
    await vi.waitFor(async () => {
      expect((await ctx.rainyMemory.status({ workspaceId })).items).toHaveLength(1)
    })
    await vi.waitFor(async () => {
      expect(await ctx.rainyMemory.status({ workspaceId })).toMatchObject({ pending: false, generating: false })
    })
    expect(adapter.requests.filter(request => request.purpose === 'project-memory')).toHaveLength(1)
    const request = adapter.requests.find(request => request.purpose === 'project-memory')!
    expect(request.tools).toBeUndefined()
    expect(request.maxTokens).toBe(512)
    expect(estimateRequest(request)).toBeLessThanOrEqual(4096)
    const events = agent.session.snapshotEvents()
    expect(events.some(event => event.type === 'rainy/memory-request')).toBe(true)
    expect(events.some(event => event.type === 'rainy/memory-result' && event.data.status === 'committed')).toBe(true)
    await ctx.rainyMemory.setEnabled({ workspaceId, generationEnabled: false })
    const next = await loop.create(SessionId('memory-next'), { provider: 'mock', model: 'model' }, { cwd: projectPath })
    next.followup(followup('Continue.'))
    await next.whenIdle()
    expect(
      next.session.deriveMessages().filter(message => ctx.rainyMemory.recallMessageIds(next.id).includes(message.id)),
    ).toHaveLength(1)
    const recall = next.session
      .deriveMessages()
      .find(message => ctx.rainyMemory.recallMessageIds(next.id).includes(message.id))
    expect(JSON.stringify(recall)).toContain('memory.v1.json')
    expect(await ctx.rainyMemory.status({ workspaceId })).toMatchObject({ enabled: true, generationEnabled: false })
  })

  it('cancels an in-flight auxiliary stream immediately for new foreground input without losing old state', async () => {
    const adapter = new MockAdapter([textResponse('First.'), 'hang', textResponse('Second.')])
    const { ctx, agent, workspaceId } = await harness(adapter)
    agent.followup(followup('Remember the project constraint.'))
    await agent.whenIdle()
    await vi.waitFor(() => {
      expect(adapter.requests.some(request => request.purpose === 'project-memory')).toBe(true)
    })
    const auxiliary = adapter.requests.find(request => request.purpose === 'project-memory')!
    agent.followup(followup('Foreground work now.'))
    await agent.whenIdle()
    expect(auxiliary.signal?.aborted).toBe(true)
    await vi.waitFor(() => {
      expect(
        agent.session
          .snapshotEvents()
          .some(event => event.type === 'rainy/memory-result' && event.data.status === 'cancelled'),
      ).toBe(true)
    })
    expect((await ctx.rainyMemory.status({ workspaceId })).revision).toBe(0)
    await ctx.rainyMemory.setEnabled({ workspaceId, generationEnabled: false })
  })

  it('preserves the last revision after invalid output and excludes other-target environment notes from recall', async () => {
    const adapter = new MockAdapter([textResponse('Recorded.'), textResponse('not valid JSON')])
    const { ctx, agent, workspaceId, root } = await harness(adapter)
    const projectId = (await ctx.rainyMemory.status({ workspaceId })).projectId
    const store = new ProjectMemoryStore(join(root, 'project-memory'))
    const windows = { ...evidence[0], id: 'windows:1', text: 'Windows interpreter is C:/Python/python.exe.' }
    const linux = {
      ...evidence[0],
      id: 'linux:1',
      executionTargetId: 'wsl:Ubuntu',
      text: 'Linux interpreter is /opt/env/bin/python.',
    }
    const before = await store.update(projectId, current =>
      applyMemoryDelta(
        current,
        {
          notes: [
            { text: windows.text, sourceIds: [windows.id], scope: 'execution-target' },
            { text: linux.text, sourceIds: [linux.id], scope: 'execution-target' },
          ],
          remove: [],
        },
        [windows, linux],
        1024,
        new Date().toISOString(),
      ),
    )
    const recall = await ctx.rainyMemory.previewRecall(workspaceId, 20000)
    expect(recall).toContain('C:/Python/python.exe')
    expect(recall).not.toContain('/opt/env/bin/python')
    agent.followup(followup('Remember an additional project constraint.'))
    await agent.whenIdle()
    await vi.waitFor(async () => {
      expect((await ctx.rainyMemory.status({ workspaceId })).error).toBeDefined()
    })
    const after = await store.read(projectId)
    expect(after.revision).toBe(before.revision)
    expect(after.items).toEqual(before.items)
    expect(after.lastAttemptAt).toBeGreaterThan(0)
    const auxiliary = adapter.requests.find(request => request.purpose === 'project-memory')
    expect(JSON.stringify(auxiliary?.messages)).not.toContain('/opt/env/bin/python')
    await ctx.rainyMemory.setEnabled({ workspaceId, generationEnabled: false })
  })

  it('defers for runtime activity and drains a cancelled auxiliary request before disposing the session', async () => {
    let busy = true
    const hasActivity = vi.fn(() => busy)
    const adapter = new MockAdapter([textResponse('Recorded.'), 'hang-slow'])
    const { ctx, agent, handle, workspaceId } = await harness(adapter, { hasActivity })
    agent.followup(followup('Remember this project constraint.'))
    await agent.whenIdle()
    await vi.waitFor(() => {
      expect(hasActivity).toHaveBeenCalled()
    })
    expect(adapter.requests.filter(request => request.purpose === 'project-memory')).toHaveLength(0)
    expect((await ctx.rainyMemory.status({ workspaceId })).pending).toBe(true)
    busy = false
    await vi.waitFor(() => {
      expect(adapter.requests.some(request => request.purpose === 'project-memory')).toBe(true)
    })
    const cancelled: string[] = []
    ctx.on('session/event', (_session, event) => {
      if (event.type === 'rainy/memory-result') cancelled.push(event.data.status)
    })
    await handle.dispose()
    expect(cancelled).toEqual(['cancelled'])
    expect(ctx.agents.get(agent.id)).toBeUndefined()
    expect((await ctx.rainyMemory.status({ workspaceId })).revision).toBe(0)
  })

  it('restores the recall marker and source watermarks without regenerating old evidence', async () => {
    const first = await harness(
      new MockAdapter([
        textResponse('Recorded.'),
        (request) => {
          const input = z.object({ evidence: z.array(z.object({ id: z.string() })) }).parse(
            JSON.parse(
              request.messages[0].content
                .filter(block => block.type === 'text')
                .map(block => block.text)
                .join(''),
            ),
          )
          return textResponse(
            JSON.stringify({ notes: [{ text: 'Use Python 3.12.', sourceIds: [input.evidence[0].id] }], remove: [] }),
          )
        },
        textResponse('Continued.'),
      ]),
      { persist: true },
    )
    first.agent.followup(followup('Use Python 3.12.'))
    await first.agent.whenIdle()
    await vi.waitFor(async () => {
      expect((await first.ctx.rainyMemory.status({ workspaceId: first.workspaceId })).items).toHaveLength(1)
    })
    await first.ctx.rainyMemory.setEnabled({ workspaceId: first.workspaceId, generationEnabled: false })
    const next = await first.ctx.agents.create({
      sessionId: SessionId('memory-next'),
      agentOptions: { provider: 'mock', model: 'model' },
      meta: { cwd: first.projectPath },
    })
    next.agent.followup(followup('Continue.'))
    await next.agent.whenIdle()
    const revision = (await first.ctx.rainyMemory.status({ workspaceId: first.workspaceId })).revision
    const generationEvents = first.agent.session.snapshotEvents()
      .filter(event => event.type === 'rainy/memory-request' || event.type === 'rainy/memory-result')
    expect(generationEvents.map(event => event.type)).toEqual(['rainy/memory-request', 'rainy/memory-result'])
    await next.dispose()
    await first.handle.dispose()
    await first.ctx.fiber.dispose()
    const adapter = new MockAdapter([])
    const generatorRestored = await harness(adapter, { root: first.root, persist: true, resume: true, sessionId: 'memory-session' })
    expect(generatorRestored.agent.session.snapshotEvents()
      .filter(event => event.type === 'rainy/memory-request' || event.type === 'rainy/memory-result')).toEqual(generationEvents)
    const request = generationEvents.find(event => event.type === 'rainy/memory-request')
    const generationMessageIds = request?.data.messages.map(message => message.id) ?? []
    expect(generatorRestored.agent.session.deriveMessages().some(message => generationMessageIds.includes(message.id))).toBe(false)
    await generatorRestored.handle.dispose()
    await generatorRestored.ctx.fiber.dispose()
    const restored = await harness(adapter, { root: first.root, persist: true, resume: true, sessionId: 'memory-next' })
    const before = restored.ctx.sessionProjections.stateOf(restored.agent.session, 'rainyMemory')
    expect(before?.lastCompletedSeq).toBeGreaterThan(0)
    expect((await restored.ctx.rainyMemory.status({ workspaceId: restored.workspaceId })).revision).toBe(revision)
    const recalls = restored.agent.session
      .deriveMessages()
      .filter(message => restored.ctx.rainyMemory.recallMessageIds(restored.agent.id).includes(message.id))
    expect(recalls).toHaveLength(1)
    expect(
      restored.agent.inbox.nextStep.filter(message =>
        restored.ctx.rainyMemory.recallMessageIds(restored.agent.id).includes(message.id),
      ),
    ).toHaveLength(0)
    await restored.ctx.rainyMemory.clear({ workspaceId: restored.workspaceId })
    await restored.handle.dispose()
    await restored.ctx.fiber.dispose()
    const afterDeletion = await harness(adapter, {
      root: first.root,
      persist: true,
      resume: true,
      sessionId: 'memory-next',
    })
    expect((await afterDeletion.ctx.rainyMemory.status({ workspaceId: afterDeletion.workspaceId })).items).toEqual([])
    expect(adapter.requests).toHaveLength(0)
  })
})
