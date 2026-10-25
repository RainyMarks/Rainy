// @vitest-environment happy-dom
/** The map page finds its project: loading, found, gone from the Host's list, or a list that failed and can be retried. */
import { act } from 'react'
import { createRoot, type Root } from 'react-dom/client'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { IdeWorkspace, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { HostMethod } from '../../src/shared/rpc.ts'
import { mapMessages } from '../../src/renderer/map/messages.ts'
import { ProjectMapView } from '../../src/renderer/map/ProjectMap.tsx'
import { lookupIn, useWorkspaceLookup, type WorkspaceLookup } from '../../src/renderer/map/workspace-lookup.ts'
import { host } from './ide-host-mock.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))
;(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true

const id = brandString<WorkspaceId>('workspace-a')
const project: IdeWorkspace = { workspaceId: id, path: 'C:\\work\\a', title: 'a' }
const other: IdeWorkspace = { workspaceId: brandString<WorkspaceId>('workspace-b'), path: 'C:\\work\\b', title: 'b' }
const pending = (): Promise<never> => new Promise(() => undefined)
/** Lets the Host mock's answers travel through `callIde` and into state. */
const settle = async (): Promise<void> => { await act(async () => { await new Promise(resolve => setTimeout(resolve, 0)) }) }

/** Answer `workspaces.list` with each of `lists` in turn (an Error rejects); every other call stays pending. */
function answerLists(...lists: readonly (readonly IdeWorkspace[] | Error | 'pending')[]): void {
  const queue = [...lists]
  host.call.mockImplementation((method: HostMethod) => {
    if (method !== 'ide') return pending()
    const next = queue.shift() ?? 'pending'
    return next === 'pending' ? pending() : next instanceof Error ? Promise.reject(next) : Promise.resolve(next)
  })
}
const listCalls = (): number => host.call.mock.calls.filter(([method]) => method === 'ide').length

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
})

describe('useWorkspaceLookup', () => {
  const seen: WorkspaceLookup[] = []
  function Probe({ workspaceId }: { workspaceId: WorkspaceId }): null {
    seen.push(useWorkspaceLookup(workspaceId))
    return null
  }
  const last = (): WorkspaceLookup => seen.at(-1)!

  it('finds the project in the list, or says it is missing', () => {
    expect(lookupIn([other, project], id)).toEqual({ status: 'ready', workspace: project })
    expect(lookupIn([other], id)).toEqual({ status: 'missing' })
  })

  it('is loading until the list arrives, then has the project', async () => {
    answerLists([project])
    mount(<Probe workspaceId={id} />)
    expect(last()).toEqual({ status: 'loading' })
    await settle()
    expect(last()).toEqual({ status: 'ready', workspace: project })
  })

  it('keeps the failure and asks again on retry', async () => {
    answerLists(new Error('socket closed'), [project])
    mount(<Probe workspaceId={id} />)
    await settle()
    const failed = last()
    expect(failed).toMatchObject({ status: 'failed', message: 'socket closed' })
    act(() => { (failed as Extract<WorkspaceLookup, { status: 'failed' }>).retry() })
    expect(last()).toEqual({ status: 'loading' })
    await settle()
    expect(last()).toEqual({ status: 'ready', workspace: project })
    expect(listCalls()).toBe(2)
  })

  it('drops the answer for a project the page no longer shows', async () => {
    let answerFirst: (list: readonly IdeWorkspace[]) => void = () => undefined
    host.call.mockImplementationOnce(() => new Promise((resolve) => { answerFirst = resolve }))
    host.call.mockImplementationOnce(() => Promise.resolve([other]))
    mount(<Probe workspaceId={id} />)
    act(() => { roots.at(-1)!.render(<Probe workspaceId={other.workspaceId} />) })
    await settle()
    answerFirst([project])
    await settle()
    expect(last()).toEqual({ status: 'ready', workspace: other })
  })
})

describe('the map page', () => {
  const t = mapMessages.t
  const page = (): JSX.Element => <ProjectMapView workspaceId={id} visible changes={[]} activity={{ active: [], recent: [] }}
    openFile={() => undefined} ask={() => undefined} />

  it('says it is loading, not that the project is missing, while the list loads', () => {
    answerLists('pending')
    const container = mount(page())
    expect(container.textContent).toBe(t('mapLoading'))
  })

  it('says the project is gone when the Host no longer lists it', async () => {
    answerLists([other])
    const container = mount(page())
    await settle()
    expect(container.textContent).toBe(t('mapProjectMissing'))
  })

  it('explains a failed list in the interface language, keeps the Host\'s words as detail, and retries', async () => {
    answerLists(new Error('socket closed'), [project])
    const container = mount(page())
    await settle()
    expect(container.querySelector('[role="alert"]')?.textContent).toBe(`${t('mapProjectFailed')}socket closed${t('mapRetry')}`)
    act(() => { container.querySelector('button')!.click() })
    await settle()
    expect(listCalls()).toBe(2)
    expect(host.call).toHaveBeenCalledWith('map.get', { workspaceId: id })
    expect(container.textContent).toBe(t('mapLoading'))
  })
})
