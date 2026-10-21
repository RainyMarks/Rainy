// @vitest-environment happy-dom
/** Files and folders handed over by Explorer open in the project that holds them, or with their folder as a new project. */
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { IdeFileDocument, IdeFileVersion, IdeWorkspace, IdeWorkspaceState, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { IdeApiRequest, IdeFilesApi } from '../../src/renderer/ide/ide-api.ts'
import type { IdeExecutionApi } from '../../src/renderer/ide/ide-execution-api.ts'
import { createWorkbench, parentDirectory, type LaunchEntry, type Workbench } from '../../src/renderer/app/workbench.ts'
import { waitFor } from './ide-dom.tsx'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

const notes: IdeWorkspace = { workspaceId: 'notes' as WorkspaceId, path: 'C:\\Users\\me\\Notes', title: 'Notes' }
const state = (): IdeWorkspaceState => ({
  version: 1, revision: 0,
  data: { lastSessionId: null, tabs: [], activePath: null, expandedPaths: [], buffers: [],
    layout: { sidebarWidth: 240, agentWidth: 0, bottomHeight: 240, sidebarVisible: true, agentVisible: false, bottomVisible: false, bottomTab: 'terminal' } },
})

const workbenches: Workbench[] = []
afterEach(async () => { await Promise.all(workbenches.splice(0).map(workbench => workbench.dispose())) })

/** A workbench whose Host knows `notes`, opens any folder as a new project, and reads every file as one line of text. */
function launch(batches: LaunchEntry[][]) {
  const projects = [notes]
  const request = vi.fn(async (body: IdeApiRequest): Promise<unknown> => {
    switch (body.op) {
      case 'workspaces.list': return projects
      case 'state.selection.read': return { version: 1, workspaceId: null }
      case 'state.selection.save': return { version: 1, workspaceId: body.workspaceId }
      case 'workspaces.open': {
        const project = projects.find(entry => entry.path === body.path)
          ?? { workspaceId: `p${projects.length}` as WorkspaceId, path: body.path, title: parentDirectory(`${body.path}\\x`).split('\\').at(-1) ?? body.path }
        if (!projects.includes(project)) projects.push(project)
        return project
      }
      case 'state.read': return state()
      case 'state.save': return { ...state(), revision: body.baseRevision + 1, data: body.data }
      case 'files.list': return { path: body.path, entries: [] }
      case 'files.changes': return []
      case 'files.read': return { workspaceId: body.workspaceId, path: body.path, content: '# Title\n', version: 'v1' as IdeFileVersion, bytes: 8, bom: false,
        eol: 'lf', readOnlyReason: null } satisfies IdeFileDocument
      default: throw new Error(`Unexpected ${body.op}`)
    }
  })
  let notify: (() => void) | undefined
  const take = vi.fn(async (): Promise<readonly LaunchEntry[]> => batches.shift() ?? [])
  const workbench = createWorkbench({
    files: { request: request as unknown as IdeFilesApi['request'] },
    // Like the Host adapter, an unanswered request rejects once it is cancelled.
    execution: { request: vi.fn((_body: unknown, signal?: AbortSignal) => new Promise((_resolve, reject) => {
      signal?.addEventListener('abort', () => { reject(new DOMException('cancelled', 'AbortError')) }, { once: true })
    })) as unknown as IdeExecutionApi['request'] },
    native: { takeLaunchPaths: take, onLaunchPaths: (listener) => { notify = listener; return () => { notify = undefined } } },
    sessionInWorkspace: async () => false,
  })
  workbenches.push(workbench)
  workbench.start()
  return { workbench, request, take, notify: () => { notify?.() } }
}

describe('launched files and folders', () => {
  it('opens a file inside a registered project in that project', async () => {
    const { workbench } = launch([[{ path: 'C:\\Users\\me\\Notes\\daily\\today.md', kind: 'file' }]])
    await waitFor(() => {
      expect(workbench.model.state.getSnapshot().workspace?.workspaceId).toBe('notes')
      expect(workbench.model.state.getSnapshot().data.activePath).toBe('daily/today.md')
    })
  })

  it('opens a file elsewhere with its folder as a new project, and a folder as a project', async () => {
    const { workbench, request, notify } = launch([[{ path: 'D:\\Writing\\draft.md', kind: 'file' }], [{ path: 'E:\\Code', kind: 'directory' }]])
    await waitFor(() => {
      expect(workbench.model.state.getSnapshot().workspace?.path).toBe('D:\\Writing')
      expect(workbench.model.state.getSnapshot().data.activePath).toBe('draft.md')
    })
    // A later launch while the window is open.
    notify()
    await waitFor(() => { expect(workbench.model.state.getSnapshot().workspace?.path).toBe('E:\\Code') })
    expect(request.mock.calls.filter(([body]) => body.op === 'workspaces.open').map(([body]) => (body as { path: string }).path)).toEqual(['D:\\Writing', 'E:\\Code'])
  })

  it('finds the parent folder of Windows, UNC and POSIX paths', () => {
    expect(parentDirectory('C:\\notes.md')).toBe('C:\\')
    expect(parentDirectory('C:\\Users\\me\\a.md')).toBe('C:\\Users\\me')
    expect(parentDirectory('\\\\wsl.localhost\\Ubuntu\\home\\me\\a.md')).toBe('\\\\wsl.localhost\\Ubuntu\\home\\me')
    expect(parentDirectory('/home/me/a.md')).toBe('/home/me')
    expect(parentDirectory('/a.md')).toBe('/')
  })
})
