// @vitest-environment happy-dom
/** The project map page: stack, architecture boxes, details, opening a directory, the AI explanation and the activity list. */
import { act } from 'react'
import { afterEach, beforeEach, expect, it, vi } from 'vitest'
import type { IdeWorkspace, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { MapExplanation, MapFile, ProjectMap } from '../../src/shared/map-protocol.ts'
import type { AiActivity } from '../../src/renderer/map/activity.ts'
import { ProjectMapView } from '../../src/renderer/map/ProjectMap.tsx'
import { all, button, cleanup, click, fakeHost, flush, handle, hasText, render, waitFor } from './settings-harness.tsx'

vi.mock('../../src/renderer/rpc.ts', async () => (await import('./settings-harness.tsx')).rpcModule)
vi.mock('../../src/renderer/ui/toasts.tsx', async () => (await import('./settings-harness.tsx')).toastsModule)

const workspace: IdeWorkspace = { workspaceId: 'w1' as WorkspaceId, path: '/ctf/tool', title: 'tool' }

function project(): ProjectMap {
  const paths: [string, number, MapFile['kind']][] = [['README.md', 20, 'doc']]
  for (const part of ['app', 'core', 'util']) for (let index = 0; index < 9; index++) paths.push([`src/${part}/m${index}.py`, part === 'core' ? 300 : 100, 'code'])
  const files: MapFile[] = paths.map(([path, lines, kind]) => ({ path, lines, kind, language: kind === 'doc' ? 'markdown' : 'python', bytes: lines * 30 }))
  const index = (path: string): number => files.findIndex(file => file.path === path)
  return {
    workspaceId: workspace.workspaceId, title: 'tool', generatedAt: 0, truncated: false, files,
    imports: [index('src/app/m0.py'), index('src/core/m0.py'), index('src/core/m1.py'), index('src/util/m0.py'), index('src/app/m1.py'), index('src/util/m2.py')],
    packages: [{ name: 'pwn', files: [index('src/core/m0.py')] }],
    stack: [{ name: 'Python', kind: 'language' }, { name: 'pwntools', kind: 'library' }],
  }
}

let explanation: MapExplanation
beforeEach(() => {
  explanation = { generatedAt: 0, locale: 'zh', summary: '一个解题工具。', modules: { 'src/core': '核心求解逻辑' }, model: 'fake/fake-model' }
  handle('map.get', () => project())
  handle('map.explain', () => explanation)
})
afterEach(async () => {
  await cleanup()
  fakeHost.call.mockClear()
})

const nodes = (): string[] => all('[data-map-node]').map(node => node.getAttribute('data-map-node')!)

/** SVG elements have no `click()`; dispatch the event React listens for. */
async function select(id: string): Promise<void> {
  await act(async () => { document.querySelector(`[data-map-node="${id}"]`)!.dispatchEvent(new MouseEvent('click', { bubbles: true })) })
  await flush()
}

it('draws the parts of the project with its stack, explains them, and shows a part\'s details', async () => {
  const openFile = vi.fn()
  const ask = vi.fn()
  await render(<ProjectMapView workspace={workspace} visible changes={[]} activity={{ active: [], recent: [] }} openFile={openFile} ask={ask} />)
  await waitFor(() => { expect(nodes()).toEqual(['src/app', 'src/core', 'src/util']) })
  expect(hasText('pwntools')).toBe(true)
  expect(hasText('28 个文件 · 27 个代码文件 · 4.5k 行代码')).toBe(true)
  // Docs are hidden until asked for.
  await click(document.querySelector<HTMLInputElement>('input[type="checkbox"]')!)
  expect(nodes()).toContain('README.md')

  await click(button('AI 讲解'))
  await waitFor(() => { expect(hasText('一个解题工具。')).toBe(true) })
  expect(fakeHost.call).toHaveBeenCalledWith('map.explain', { workspaceId: 'w1' })
  expect(document.querySelector('[data-map-node="src/core"]')?.textContent).toContain('核心求解逻辑')

  await select('src/core')
  const details = document.querySelector<HTMLElement>('[data-map-details]')!
  expect(details.textContent).toContain('核心求解逻辑')
  expect(details.textContent).toContain('src/app')
  expect(details.textContent).toContain('src/util')
  expect(details.textContent).toContain('pwn')
  await click(button('m0.py', details))
  expect(openFile).toHaveBeenCalledWith('src/core/m0.py')
  await click(button('让 AI 解释这部分', details))
  expect(ask.mock.calls[0]![0]).toContain('`src/core`')

  await click(button('进入', details))
  await waitFor(() => { expect(nodes().filter(id => id.startsWith('src/core/'))).toHaveLength(9) })
  expect(nodes()).toEqual(expect.arrayContaining(['src/app', 'src/util']))
  await click(button('返回'))
  await waitFor(() => { expect(nodes()).toEqual(['src/app', 'src/core', 'src/util', 'README.md']) })
})

it('shows what the agent is doing, on the parts and in the activity list', async () => {
  const active: AiActivity[] = [{ sessionId: 's', toolCallId: 't1', action: 'edit', path: '/ctf/tool/src/util/m3.py', label: 'src/util/m3.py', startedAt: Date.now() }]
  const recent: AiActivity[] = [{ sessionId: 's', toolCallId: 't0', action: 'read', path: '/ctf/tool/src/app/m1.py', label: 'src/app/m1.py', startedAt: Date.now() - 5000, endedAt: Date.now() - 4000 }]
  const changes = [{ path: '/ctf/tool/src/app/m2.py', display: 'src/app/m2.py', toolCallIds: ['x'], created: false, deleted: false, added: 1, removed: 1, comparable: true, undoable: true }]
  await render(<ProjectMapView workspace={workspace} visible changes={changes} activity={{ active, recent }} openFile={vi.fn()} ask={vi.fn()} />)
  await waitFor(() => { expect(nodes()).toHaveLength(3) })
  expect(document.querySelector('[data-map-node="src/util"]')?.getAttribute('data-mark')).toBe('writing')
  expect(document.querySelector('[data-map-node="src/app"]')?.textContent).toContain('✎1')
  const feed = document.querySelector<HTMLElement>('[data-map-activity]')!
  expect(feed.textContent).toContain('AI 改了 1 个文件，等待审查')
  expect(all('li', feed).map(item => item.textContent)).toEqual([expect.stringContaining('src/util/m3.py'), expect.stringContaining('src/app/m1.py')])
  expect(feed.querySelector('li[data-live]')?.textContent).toContain('进行中')
})
