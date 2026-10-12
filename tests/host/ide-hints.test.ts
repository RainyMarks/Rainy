/** Change hints: throttled per workspace and kind, sent when a watched directory changes, per project folder and leased. */
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { IdeRootId, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { IdeHints } from '../../src/host/ide/hints.ts'

const workspace = brandString<WorkspaceId>('w1')
const primary = brandString<IdeRootId>('primary')
const attached = brandString<IdeRootId>('attached')
const closers: (() => Promise<void> | void)[] = []
afterEach(async () => {
  for (const close of closers.splice(0).reverse()) await close()
  vi.useRealTimers()
})

it('sends the first hint at once and folds a burst into one more', async () => {
  vi.useFakeTimers()
  const sent: string[] = []
  const hints = new IdeHints((id, kind) => { sent.push(`${kind}:${id}`) }, 60_000)
  closers.push(() => { hints.close() })
  for (let index = 0; index < 20; index++) hints.notify(workspace, 'execution')
  hints.notify(workspace, 'files')
  expect(sent).toEqual(['execution:w1', 'files:w1'])
  await vi.advanceTimersByTimeAsync(100)
  expect(sent).toEqual(['execution:w1', 'files:w1', 'execution:w1'])
  await vi.advanceTimersByTimeAsync(500)
  expect(sent).toHaveLength(3)
})

it('hints when a file in a watched directory changes, and stops after the set is replaced', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'rainy-hints-'))
  closers.push(() => rm(directory, { recursive: true, force: true }))
  const sent: string[] = []
  const hints = new IdeHints((id, kind) => { sent.push(`${kind}:${id}`) }, 60_000)
  closers.push(() => { hints.close() })
  hints.watchDirectories(workspace, primary, [directory])
  await writeFile(join(directory, 'solve.py'), 'print(1)\n')
  await vi.waitFor(() => { expect(sent).toContain('files:w1') })
  hints.watchDirectories(workspace, primary, [])
  await new Promise(done => setTimeout(done, 200))
  sent.length = 0
  await writeFile(join(directory, 'solve.py'), 'print(2)\n')
  await new Promise(done => setTimeout(done, 200))
  expect(sent).toEqual([])
})

it('keeps the watchers of each project folder apart and closes them when the page stops checking', async () => {
  const first = await mkdtemp(join(tmpdir(), 'rainy-hints-'))
  const second = await mkdtemp(join(tmpdir(), 'rainy-hints-'))
  closers.push(() => rm(first, { recursive: true, force: true }), () => rm(second, { recursive: true, force: true }))
  const sent: string[] = []
  const hints = new IdeHints((id, kind) => { sent.push(`${kind}:${id}`) }, 1000)
  closers.push(() => { hints.close() })
  hints.watchDirectories(workspace, primary, [first])
  hints.watchDirectories(workspace, attached, [second])
  await writeFile(join(first, 'a.txt'), 'a')
  await vi.waitFor(() => { expect(sent).toContain('files:w1') })
  await new Promise(done => setTimeout(done, 200))
  sent.length = 0
  await writeFile(join(second, 'b.txt'), 'b')
  await vi.waitFor(() => { expect(sent).toContain('files:w1') })
  await new Promise(done => setTimeout(done, 1200))
  sent.length = 0
  await writeFile(join(first, 'a.txt'), 'again')
  await writeFile(join(second, 'b.txt'), 'again')
  await new Promise(done => setTimeout(done, 300))
  expect(sent).toEqual([])
})
