// @vitest-environment happy-dom
/** The project tour and the AI footprint: what they show and where their links lead. */
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { IdeWorkspace, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { FootprintTurn, MapExplanation } from '../../src/shared/map-protocol.ts'
import type { HostMethod, SessionId } from '../../src/shared/rpc.ts'
import { FootprintView } from '../../src/renderer/map/Footprint.tsx'
import { mapMessages } from '../../src/renderer/map/messages.ts'
import { Inline, TourView } from '../../src/renderer/map/Tour.tsx'
import { host } from './ide-host-mock.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

const t = mapMessages.t
const settle = async (): Promise<void> => { await act(async () => { await new Promise(resolve => setTimeout(resolve, 0)) }) }
const roots: Root[] = []
function mount(element: JSX.Element): HTMLElement {
  const container = document.createElement('div')
  document.body.append(container)
  const root = createRoot(container)
  roots.push(root)
  act(() => { root.render(element) })
  return container
}
afterEach(() => {
  for (const root of roots.splice(0)) act(() => { root.unmount() })
  document.body.replaceChildren()
  host.call.mockReset()
  host.on.mockClear()
})

const buttons = (container: HTMLElement): HTMLButtonElement[] => [...container.querySelectorAll('button')]
const button = (container: HTMLElement, text: string): HTMLButtonElement => buttons(container).find(item => item.textContent === text)!

describe('Inline', () => {
  it('shows backticked text as code and leaves an unclosed backtick as text', () => {
    const container = mount(<p><Inline text="run `npm test` then `x" /></p>)
    expect(container.querySelector('code')?.textContent).toBe('npm test')
    expect(container.textContent).toBe('run npm test then `x')
  })
})

describe('the tour', () => {
  const explanation: MapExplanation = {
    generatedAt: 0, locale: 'zh', summary: '一个 `Rust` 项目。', model: 'deepseek/deepseek-chat',
    modules: { src: '源码', 'src/main.rs': '入口' },
    run: ['`cargo run`'],
    start: [{ path: 'src/main.rs', line: 3, text: '从入口读起' }],
    flows: [{ title: '启动', steps: [{ path: 'src/app', text: '应用目录' }, { text: '没有文件的一步' }] }],
    tasks: [{ task: '加一个命令', places: [{ text: '命令表在哪里不确定' }] }],
    drift: { added: 2, removed: 1 },
  }
  const isDirectory = (path: string): boolean => path === 'src' || path === 'src/app'

  it('invites the user to have it written when there is none', () => {
    const onGenerate = vi.fn()
    const container = mount(<TourView explanation={undefined} explaining={false} isDirectory={isDirectory} onGenerate={onGenerate}
      onOpenFile={() => undefined} onShowDirectory={() => undefined} t={t} />)
    expect(container.textContent).toContain(t('tourIntro'))
    act(() => { button(container, t('tourGenerate')).click() })
    expect(onGenerate).toHaveBeenCalledOnce()
  })

  it('opens files at their line, shows directories in the structure view and says when it is out of date', () => {
    const onOpenFile = vi.fn()
    const onShowDirectory = vi.fn()
    const container = mount(<TourView explanation={explanation} explaining={false} isDirectory={isDirectory} onGenerate={() => undefined}
      onOpenFile={onOpenFile} onShowDirectory={onShowDirectory} t={t} />)
    expect(container.textContent).toContain(t('tourDrift', { added: 2, removed: 1 }))
    expect(container.textContent).toContain(t('tourNoFile'))
    act(() => { button(container, 'main.rs:3').click() })
    expect(onOpenFile).toHaveBeenCalledWith('src/main.rs', 3)
    act(() => { button(container, 'src/app/').click() })
    expect(onShowDirectory).toHaveBeenCalledWith('src/app')
    // Only folders are listed among the parts.
    expect(buttons(container).some(item => item.textContent === 'src/main.rs')).toBe(false)
    act(() => { button(container, 'src/').click() })
    expect(onShowDirectory).toHaveBeenLastCalledWith('src')
  })

  it('asks for a new tour when the kept one only has a summary', () => {
    const container = mount(<TourView explanation={{ generatedAt: 0, locale: 'zh', summary: 's', modules: {}, model: 'm' }} explaining={false}
      isDirectory={isDirectory} onGenerate={() => undefined} onOpenFile={() => undefined} onShowDirectory={() => undefined} t={t} />)
    expect(container.textContent).toContain(t('tourOld'))
  })
})

describe('the footprint', () => {
  const workspace: IdeWorkspace = { workspaceId: 'w' as WorkspaceId, path: 'C:\\work\\a', title: 'a' }
  const turns: FootprintTurn[] = [
    { id: 'u1', text: '先看看', ts: 1, steps: [] },
    { id: 'u2', text: '修登录', ts: 2, steps: [
      { action: 'read', label: 'src/login.ts', path: 'C:\\work\\a\\src\\login.ts', ok: true },
      { action: 'search', label: 'token', path: 'C:\\work\\a\\src', ok: true },
      { action: 'edit', label: 'src/login.ts', path: 'C:\\work\\a\\src\\login.ts', ok: true },
      { action: 'command', label: 'Run the tests' },
    ] },
  ]

  it('shows the newest turn with its counts and steps, and goes back to earlier turns', async () => {
    host.call.mockImplementation((method: HostMethod) => method === 'map.footprint' ? Promise.resolve({ turns }) : new Promise(() => undefined))
    const onOpenFile = vi.fn()
    const container = mount(<FootprintView workspace={workspace} sessionId={'s1' as SessionId} onOpenFile={onOpenFile} t={t} />)
    await settle()
    expect(host.call).toHaveBeenCalledWith('map.footprint', { sessionId: 's1' })
    expect(container.textContent).toContain('修登录')
    expect(container.textContent).toContain(t('footprintCountRead', { count: 1 }))
    expect(container.textContent).toContain(t('footprintCountChanged', { count: 1 }))
    expect(container.textContent).toContain(t('footprintRunning'))
    expect(container.textContent).toContain('token· src')
    act(() => { buttons(container).find(item => item.textContent === 'src/login.ts')!.click() })
    expect(onOpenFile).toHaveBeenCalledWith('src/login.ts')
    act(() => { button(container, t('footprintPrevious')).click() })
    expect(container.textContent).toContain(t('footprintNoSteps'))
    expect(host.on).toHaveBeenCalledWith('session.tool', expect.any(Function))
  })

  it('asks for a chat when none is selected', () => {
    const container = mount(<FootprintView workspace={workspace} sessionId={null} onOpenFile={() => undefined} t={t} />)
    expect(container.textContent).toBe(t('footprintNoChat'))
    expect(host.call).not.toHaveBeenCalled()
  })
})
