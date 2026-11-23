// @vitest-environment happy-dom
/** Component tests for HexViewer narrow-pane toolbar folding and ProjectMap adaptive 3+ digit change badges. */
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { IdeWorkspace, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ProjectMap } from '../../src/shared/map-protocol.ts'
import type { ChatChange, HostMethod } from '../../src/shared/rpc.ts'
import { HexViewer } from '../../src/renderer/ide/viewers/HexViewer.tsx'
import { NODE_WIDTH } from '../../src/renderer/map/layout.ts'
import { changeBadgeLayout, ProjectMapView } from '../../src/renderer/map/ProjectMap.tsx'
import { host } from './ide-host-mock.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

const roots: Root[] = []
function mount(element: JSX.Element): HTMLElement {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  roots.push(root)
  act(() => { root.render(element) })
  return container
}

async function flush(ms = 30): Promise<void> {
  await act(async () => { await new Promise(resolve => setTimeout(resolve, ms)) })
}

function mockNarrowMatchMedia(narrow: boolean): void {
  vi.spyOn(window, 'matchMedia').mockImplementation((query: string) => ({
    matches: query.includes('max-width') ? narrow : false,
    media: query,
    onchange: null,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    addListener: () => undefined,
    removeListener: () => undefined,
    dispatchEvent: () => false,
  }) as MediaQueryList)
}

afterEach(() => {
  for (const root of roots.splice(0)) act(() => { root.unmount() })
  document.body.replaceChildren()
  host.call.mockReset()
  vi.restoreAllMocks()
})

describe('HexViewer narrow-pane toolbar', () => {
  const stubFetch = (): void => {
    const payload = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01, 0x02, 0x03])
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(payload, { status: 206, headers: { etag: '"v1"' } }))
  }

  it('shows inline copy select on wide panes and folds copy actions into the More menu on narrow panes', async () => {
    stubFetch()
    mockNarrowMatchMedia(false)
    const wide = mount(<HexViewer url="/rainy/ide/raw?w=w&p=a.bin" size={12} onChanged={() => undefined} />)
    await flush()
    expect(wide.querySelector('[data-narrow]')).toBeNull()
    expect(wide.querySelector('[data-tool-more]')).toBeNull()
    expect(wide.querySelectorAll('select')).toHaveLength(2)

    mockNarrowMatchMedia(true)
    const narrow = mount(<HexViewer url="/rainy/ide/raw?w=w&p=a.bin" size={12} onChanged={() => undefined} />)
    await flush()
    expect(narrow.querySelector('[data-narrow]')).not.toBeNull()
    // Copy select moved into ToolActions ("More") menu; only the encoding select remains inline.
    expect(narrow.querySelectorAll('select')).toHaveLength(1)
    const more = narrow.querySelector<HTMLButtonElement>('[data-tool-more]')
    expect(more).not.toBeNull()
    act(() => { more!.click() })
    const items = [...document.body.querySelectorAll('[role="menuitem"]')]
    expect(items).toHaveLength(5)
  })
})

describe('ProjectMap change badge layout and rendering', () => {
  it('expands badge width for 2/3-digit counts and caps 1000+ at ✎999+ inside node bounds', () => {
    const single = changeBadgeLayout(5)
    const double = changeBadgeLayout(42)
    const triple = changeBadgeLayout(128)
    const huge = changeBadgeLayout(1200)
    expect(single.label).toBe('✎5')
    expect(double.label).toBe('✎42')
    expect(triple.label).toBe('✎128')
    expect(huge.label).toBe('✎999+')
    expect(triple.width).toBeGreaterThan(26)
    expect(huge.width).toBeGreaterThanOrEqual(triple.width)
    for (const badge of [single, double, triple, huge]) {
      expect(badge.x).toBeGreaterThanOrEqual(0)
      expect(badge.x + badge.width).toBe(NODE_WIDTH - 8)
      expect(badge.textX).toBe(badge.width / 2)
    }
  })

  it('renders an expanded SVG badge for a directory with 128 changed files without overflowing', async () => {
    const workspaceId = 'ws-badge' as WorkspaceId
    const workspace: IdeWorkspace = { workspaceId, path: 'C:\\repo', title: 'repo' }
    const files = [
      ...Array.from({ length: 128 }, (_unused, index) => ({
        path: `src/f${index}.ts`,
        language: 'typescript',
        kind: 'code' as const,
        lines: 20,
        bytes: 400,
      })),
      { path: 'lib/util.ts', language: 'typescript', kind: 'code' as const, lines: 50, bytes: 800 },
    ]
    const map: ProjectMap = {
      workspaceId,
      title: 'repo',
      generatedAt: Date.now(),
      truncated: false,
      files,
      imports: [0, 128],
      packages: [],
      stack: [{ name: 'TypeScript', kind: 'language' }],
    }
    const changes: ChatChange[] = Array.from({ length: 128 }, (_unused, index) => ({
      path: `C:\\repo\\src\\f${index}.ts`,
      display: `src/f${index}.ts`,
      toolCallIds: [`call-${index}`],
      created: false,
      deleted: false,
      added: 1,
      removed: 0,
      comparable: true,
      undoable: true,
    }))
    host.call.mockImplementation((method: HostMethod) => {
      if (method === 'ide') return Promise.resolve([workspace])
      if (method === 'map.get') return Promise.resolve(map)
      return new Promise(() => undefined)
    })

    const container = mount(
      <ProjectMapView workspaceId={workspaceId} visible sessionId={null} changes={changes}
        activity={{ active: [], recent: [] }} openFile={() => undefined} ask={() => undefined} />,
    )
    await flush(40)

    // Switch to the "Structure" tab to render Architecture SVG nodes.
    const structureBtn = container.querySelector<HTMLButtonElement>('#project-map-tab-structure')
    expect(structureBtn).not.toBeNull()
    act(() => { structureBtn!.click() })
    await flush(30)

    const badge = container.querySelector('g[data-change-badge]')
    expect(badge).not.toBeNull()
    const rect = badge!.querySelector('rect')!
    const text = badge!.querySelector('text')!
    expect(text.textContent).toBe('✎128')
    const width = Number(rect.getAttribute('width'))
    expect(width).toBe(changeBadgeLayout(128).width)
    expect(width).toBeGreaterThan(26)
  })
})
