/** Project map drawing: layers of the architecture, file blocks, tool paths, AI marks and terminal text for the AI. */
import { describe, expect, it } from 'vitest'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { MapFile, ProjectMap } from '../../src/shared/map-protocol.ts'
import type { ChatChange } from '../../src/shared/rpc.ts'
import { resolveToolPath, type AiActivity } from '../../src/renderer/map/activity.ts'
import { layoutGraph, NODE_HEIGHT, NODE_WIDTH } from '../../src/renderer/map/layout.ts'
import { aiMarks, markedDirectories } from '../../src/renderer/map/marks.ts'
import { layoutTreemap } from '../../src/renderer/map/treemap.ts'
import { plainTerminalText } from '../../src/renderer/ide/IdeBottom.tsx'

describe('architecture layout', () => {
  it('puts parts above what they import, breaks cycles, and keeps parts without imports in a row below', () => {
    const layout = layoutGraph(['app', 'ui', 'shared', 'util', 'docs'], [
      { from: 'app', to: 'ui', weight: 3 }, { from: 'ui', to: 'shared', weight: 5 }, { from: 'app', to: 'shared', weight: 1 },
      { from: 'shared', to: 'util', weight: 2 }, { from: 'util', to: 'shared', weight: 1 },
    ])
    const y = (id: string): number => layout.nodes.get(id)!.y
    expect(y('app')).toBeLessThan(y('ui'))
    expect(y('ui')).toBeLessThan(y('shared'))
    expect(y('docs')).toBeGreaterThan(Math.max(y('app'), y('ui'), y('shared'), y('util')))
    expect(layout.nodes.get('docs')!.isolated).toBe(true)
    expect(layout.isolatedTop).toBe(y('docs'))
    // The edge that closes the shared/util cycle points up and is drawn dashed.
    expect(layout.edges.filter(edge => edge.upward).map(edge => `${edge.from}->${edge.to}`)).toHaveLength(1)
    for (const node of layout.nodes.values()) {
      expect(node.x).toBeGreaterThanOrEqual(0)
      expect(node.x + NODE_WIDTH).toBeLessThanOrEqual(layout.width + 0.001)
      expect(node.y + NODE_HEIGHT).toBeLessThanOrEqual(layout.height + 0.001)
    }
  })

  it('wraps a wide layer and has no label row when every part stands alone', () => {
    const ids = Array.from({ length: 9 }, (_value, index) => `f${index}`)
    const layout = layoutGraph(ids, [])
    expect(new Set([...layout.nodes.values()].map(node => node.y)).size).toBe(2)
    expect(layout.isolatedTop).toBeUndefined()
  })
})

function project(paths: Record<string, number>): ProjectMap {
  const files: MapFile[] = Object.entries(paths).map(([path, lines]) => ({ path, language: 'python', kind: path.endsWith('.png') ? 'asset' : 'code', lines, bytes: lines * 40 }))
  return { workspaceId: 'w' as WorkspaceId, title: 'p', generatedAt: 0, truncated: false, files, imports: [], packages: [], stack: [] }
}

describe('file blocks', () => {
  it('fills the area with directory frames and file blocks sized by lines', () => {
    const map = project({ 'solve.py': 300, 'lib/a.py': 200, 'lib/b.py': 100, 'challenge.png': 0 })
    const blocks = layoutTreemap(map, '', 400, 300)
    const files = blocks.filter(block => block.kind === 'file')
    expect(files.map(block => block.path).sort()).toEqual(['challenge.png', 'lib/a.py', 'lib/b.py', 'solve.py'])
    for (const block of blocks) {
      expect(block.x).toBeGreaterThanOrEqual(-0.001)
      expect(block.y).toBeGreaterThanOrEqual(-0.001)
      expect(block.x + block.width).toBeLessThanOrEqual(400.001)
      expect(block.y + block.height).toBeLessThanOrEqual(300.001)
    }
    const area = (path: string): number => { const block = files.find(item => item.path === path)!; return block.width * block.height }
    expect(area('solve.py')).toBeGreaterThan(area('lib/a.py'))
    expect(blocks.find(block => block.path === 'lib')).toMatchObject({ kind: 'directory', titled: true })
    expect(layoutTreemap(map, 'lib', 400, 300).filter(block => block.kind === 'file').map(block => block.path).sort()).toEqual(['lib/a.py', 'lib/b.py'])
  })

  it('folds a directory too small to show its files', () => {
    const paths: Record<string, number> = { 'big.py': 100_000 }
    for (let index = 0; index < 20; index++) paths[`tiny/f${index}.py`] = 1
    const blocks = layoutTreemap(project(paths), '', 300, 200)
    expect(blocks.find(block => block.path === 'tiny')).toMatchObject({ kind: 'folded' })
    expect(blocks.some(block => block.path.startsWith('tiny/'))).toBe(false)
  })
})

describe('agent marks', () => {
  it('resolves tool paths against the chat directory the way the Host does', () => {
    expect(resolveToolPath('/home/rog/ctf', 'solve.py')).toBe('/home/rog/ctf/solve.py')
    expect(resolveToolPath('/home/rog/ctf', '../x/./y.py')).toBe('/home/rog/x/y.py')
    expect(resolveToolPath('C:\\ctf\\baby', 'sub/a.py')).toBe('C:\\ctf\\baby\\sub\\a.py')
    expect(resolveToolPath('C:\\ctf', 'D:\\other\\b.py')).toBe('D:\\other\\b.py')
    expect(resolveToolPath(undefined, 'a.py')).toBe('a.py')
  })

  it('marks files being written over pending changes over files being read, and the folders holding them', () => {
    const workspace = { workspaceId: 'w' as WorkspaceId, path: 'C:\\ctf\\baby', title: 'baby' }
    const activity = (action: AiActivity['action'], path: string): AiActivity => ({ sessionId: 's', toolCallId: `${action}:${path}`, action, path, label: path, startedAt: 0 })
    const change = (path: string, created = false): ChatChange => ({
      path, display: path, toolCallIds: ['c'], created, deleted: false, added: 1, removed: 0, comparable: true, undoable: true,
    })
    const marks = aiMarks(workspace, [change('C:\\ctf\\baby\\lib\\a.py'), change('C:\\ctf\\baby\\new.py', true)], [
      activity('read', 'C:\\ctf\\baby\\lib\\a.py'), activity('read', 'C:\\ctf\\baby\\solve.py'), activity('edit', 'C:\\ctf\\baby\\new.py'),
      activity('write', 'C:\\elsewhere\\x.py'),
    ])
    expect(Object.fromEntries(marks)).toEqual({ 'lib/a.py': 'changed', 'new.py': 'writing', 'solve.py': 'reading' })
    expect([...markedDirectories(marks)]).toEqual(['lib'])
  })
})

describe('terminal text for the AI', () => {
  it('drops escape sequences and keeps what carriage returns left on each line', () => {
    const raw = '\u001b]0;title\u0007\u001b[32mok\u001b[0m\r\nprogress 10%\rprogress 100%\n\u001b[2Kdone\n'
    expect(plainTerminalText(raw)).toBe('ok\nprogress 100%\ndone\n')
  })
})
