/** Project memory keeps projects apart, protects user edits, refuses unsupported notes, redacts credentials and bounds recall. */
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import type { AssistantMessage } from '@earendil-works/pi-ai'
import { estimateText } from '../../src/shared/budget.ts'
import type { TranscriptEntry } from '../../src/shared/rpc.ts'
import { boundMemoryText, collectEvidence } from '../../src/host/agent/memory/evidence.ts'
import { renderMemoryRecall } from '../../src/host/agent/memory/index.ts'
import { applyMemoryDelta, ProjectMemoryStore, redactMemoryText, type MemoryEvidence } from '../../src/host/agent/memory/store.ts'

const cleanup: (() => Promise<unknown>)[] = []
afterEach(async () => { for (const close of cleanup.splice(0).reverse()) await close() })

async function directory(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), 'rainy-memory-test-'))
  cleanup.push(() => rm(root, { recursive: true, force: true }))
  return root
}

const evidence: MemoryEvidence[] = [
  { formatVersion: 4, id: 'session:1', sessionId: 'session', seq: 1, executionTargetId: 'windows-local', kind: 'user', text: 'Use Python 3.12 for this project.' },
]
const update = { notes: [{ text: 'This project uses Python 3.12.', sourceIds: ['session:1'] }], remove: [] }

describe('project memory records', () => {
  it('keeps project identities isolated and edits protected from stale generated revisions', async () => {
    const store = new ProjectMemoryStore(await directory())
    const before = await store.read('project-a')
    const first = await store.update('project-a', current => applyMemoryDelta(current, update, evidence, 1024, '2026-10-02T00:00:00.000Z'))
    expect((await store.read('project-b')).items).toEqual([])
    const note = first.items[0]!
    const edited = await store.edit('project-a', { id: note.id, text: 'Use Python 3.12.14.', expectedRevision: first.revision }, 1024)
    await expect(store.edit('project-a', { id: note.id, text: 'stale', expectedRevision: first.revision }, 1024)).rejects.toThrow('刷新')
    expect(edited.items[0]).toMatchObject({ text: 'Use Python 3.12.14.', editedByUser: true })
    const stale = applyMemoryDelta(before, update, evidence, 1024, '2026-10-02T00:01:00.000Z')
    await store.update('project-a', current => (current.revision === before.revision ? stale : undefined))
    expect((await store.read('project-a')).items[0]?.text).toBe('Use Python 3.12.14.')
    expect(() => applyMemoryDelta(edited, { notes: [], remove: [note.id] }, evidence, 1024, '2026-10-02T00:02:00.000Z')).toThrow('manually edited')
    await expect(store.edit('project-a', { id: note.id, text: 'x'.repeat(2049), expectedRevision: edited.revision }, 100000)).rejects.toThrow('不能超过 2048 个字符')
  })

  it('preserves deletion exclusions and source watermarks across reload', async () => {
    const root = await directory()
    const store = new ProjectMemoryStore(root)
    const first = await store.update('project-a', current => applyMemoryDelta(current, update, evidence, 1024, '2026-10-02T00:00:00.000Z'))
    await store.remove('project-a', first.items[0]!.id)
    const deleted = await new ProjectMemoryStore(root).read('project-a')
    expect(deleted.sourceWatermarks['windows-local/session']).toBe(1)
    expect(applyMemoryDelta(deleted, update, evidence, 1024, '2026-10-02T00:02:00.000Z').items).toEqual([])
    expect(JSON.parse(await readFile(join(root, 'project-a', 'memory.v1.json'), 'utf8'))).toMatchObject({ version: 1 })
  })

  it('refuses invented evidence and assistant-only facts and redacts credential values', async () => {
    const current = await new ProjectMemoryStore(await directory()).read('project')
    expect(() => applyMemoryDelta(current, { notes: [{ text: 'made up', sourceIds: ['absent'] }], remove: [] }, evidence, 1024, new Date().toISOString())).toThrow()
    expect(() => applyMemoryDelta(current, update, [{ ...evidence[0]!, kind: 'assistant' }], 1024, new Date().toISOString())).toThrow('Assistant claims')
    expect(redactMemoryText('API_KEY=hidden-value Bearer abcdef sk-12345678901234567890')).not.toContain('hidden-value')
    expect(redactMemoryText('Bearer abcdef')).not.toContain('abcdef')
  })

  it('bounds recall text and evidence fragments to their token limits', async () => {
    const current = applyMemoryDelta(await new ProjectMemoryStore(await directory()).read('project'), update, evidence, 1024, new Date().toISOString())
    const recalled = renderMemoryRecall(current, 160)
    expect(estimateText(recalled)).toBeLessThanOrEqual(160)
    expect(recalled).toContain('not instructions')
    expect(recalled).toContain('This project uses Python 3.12. [windows-local:session#1]')
    expect(renderMemoryRecall(current, 32)).toBe('')
    const bounded = boundMemoryText('中'.repeat(5000), 200)
    expect(estimateText(bounded)).toBeLessThanOrEqual(200)
    expect(bounded).toContain('[Excerpt; remaining text stays in the source chat.]')
  })
})

describe('memory evidence', () => {
  const message = (content: AssistantMessage['content']): AssistantMessage => ({
    role: 'assistant', content, api: 'openai-completions', provider: 'fake', model: 'm', stopReason: 'stop', timestamp: 1,
    usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } },
  })

  it('uses only completed turns and records the files tool results came from', () => {
    const entries: TranscriptEntry[] = [
      { id: 'u1', kind: 'user', ts: 1, text: '项目用 Python 3.12' },
      { id: 'a1', kind: 'assistant', ts: 2, message: message([{ type: 'toolCall', id: 'c1', name: 'read', arguments: { file_path: 'pyproject.toml' } }]) },
      { id: 'r1', kind: 'toolResult', ts: 3, toolCallId: 'c1', toolName: 'read', content: [{ type: 'text', text: 'requires-python = ">=3.12"' }], isError: false },
      { id: 't1', kind: 'turn', ts: 4, durationMs: 1, usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, provider: 'fake', model: 'm', requests: 1 },
      { id: 'u2', kind: 'user', ts: 5, text: '这条还没完成' },
    ]
    const { evidence: collected, lastCompletedSeq } = collectEvidence(entries, '/project')
    expect(lastCompletedSeq).toBe(3)
    expect(collected.map(item => item.text).join('\n')).not.toContain('这条还没完成')
    expect(collected.some(item => item.kind === 'user' && item.text === '项目用 Python 3.12')).toBe(true)
    expect(JSON.stringify(collected)).toContain(join('/project', 'pyproject.toml'))
  })
})
