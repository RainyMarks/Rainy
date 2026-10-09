// @vitest-environment happy-dom
/** Same-named files retain their directory identity across requests, recovery and editor URIs. */
import { describe, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { IdeFilesApi } from '../../src/renderer/ide/ide-api.ts'
import { createRootedIdeApi } from '../../src/renderer/ide/ide-rooted-api.ts'
import { fileKey, fileReference, keyFromAbsolute } from '../../src/renderer/ide/ide-paths.ts'
import { sourceUri } from '../../src/renderer/ide/ide-model.ts'
import type { IdeFileVersion, IdeRootId, IdeWorkspace, IdeWorkspaceState, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

const workspaceId = 'project' as WorkspaceId
const attached = 'attached' as IdeRootId
const fileVersion = 'v1' as IdeFileVersion
const workspace: IdeWorkspace = { workspaceId, path: '/project', title: 'Project', roots: [
  { rootId: 'primary' as IdeRootId, path: '/project', title: 'Project', primary: true },
  { rootId: attached, path: '/shared tools', title: 'Tools', primary: false },
] }
const state = (): IdeWorkspaceState => ({ version: 1, revision: 3, data: {
  lastSessionId: null, tabs: [{ path: 'main.py', kind: 'file' }, { rootId: attached, path: 'main.py', kind: 'file' }],
  activePath: 'main.py', activeRootId: attached, expandedPaths: ['src'], expandedRoots: [{ rootId: attached, path: '' }],
  buffers: [{ path: 'main.py', content: 'primary draft', baseVersion: fileVersion, bom: false, eol: 'lf' },
    { rootId: attached, path: 'main.py', content: 'attached draft', baseVersion: fileVersion, bom: false, eol: 'lf' }],
  layout: { sidebarWidth: 200, agentWidth: 400, bottomHeight: 200, sidebarVisible: true, agentVisible: true, bottomVisible: false, bottomTab: 'terminal' },
} })

describe('root-qualified IDE requests', () => {
  it('decodes same-named recovery buffers and sends explicit root identities when saving', async () => {
    const request = vi.fn<IdeFilesApi['request']>().mockResolvedValueOnce(state())
    const api = createRootedIdeApi({ request: request as IdeFilesApi['request'] })
    const recovered = await api.request({ op: 'state.read', workspaceId })
    expect(recovered.data.tabs.map(tab => tab.path)).toEqual(['main.py', fileKey('main.py', attached)])
    expect(recovered.data.buffers.map(buffer => buffer.content)).toEqual(['primary draft', 'attached draft'])
    expect(recovered.data.activePath).toBe(fileKey('main.py', attached))
    request.mockResolvedValueOnce({ ...state(), revision: 4 })
    await api.request({ op: 'state.save', workspaceId, baseRevision: 3, data: recovered.data })
    const submitted = request.mock.calls.at(-1)?.[0]
    expect(submitted).toMatchObject({ op: 'state.save', data: {
      tabs: [{ path: 'main.py' }, { path: 'main.py', rootId: attached }], activePath: 'main.py', activeRootId: attached,
      expandedPaths: ['src'], expandedRoots: [{ rootId: attached, path: '' }],
    } })
    expect(JSON.stringify(submitted)).not.toContain('\\u0000')
  })

  it('keeps primary and attached observations distinct during one change poll', async () => {
    const request = vi.fn<IdeFilesApi['request']>()
      .mockResolvedValueOnce([{ path: 'main.py', version: brandString<IdeFileVersion>('primary-version'), kind: 'file' }])
      .mockResolvedValueOnce([{ path: 'main.py', version: brandString<IdeFileVersion>('attached-version'), kind: 'file' }])
    const api = createRootedIdeApi({ request: request as IdeFilesApi['request'] })
    const changes = await api.request({ op: 'files.changes', workspaceId, paths: ['main.py', fileKey('main.py', attached)] })
    expect(changes.map(change => change.path)).toEqual(['main.py', fileKey('main.py', attached)])
    expect(request).toHaveBeenNthCalledWith(1, { op: 'files.changes', workspaceId, paths: ['main.py'] })
    expect(request).toHaveBeenNthCalledWith(2, { op: 'files.changes', workspaceId, rootId: attached, paths: ['main.py'] })
  })

  it('returns attached tree keys and refuses a rename targeting a different root', async () => {
    const request = vi.fn<IdeFilesApi['request']>().mockResolvedValueOnce({ path: '', entries: [{ name: 'main.py', path: 'main.py', kind: 'file', bytes: 0, version: fileVersion, outsideWorkspace: false }] })
    const api = createRootedIdeApi({ request: request as IdeFilesApi['request'] })
    const listed = await api.request({ op: 'files.list', workspaceId, path: fileKey('', attached) })
    expect(listed.entries[0]?.path).toBe(fileKey('main.py', attached))
    expect(request).toHaveBeenCalledWith({ op: 'files.list', workspaceId, rootId: attached, path: '' })
    await expect(api.request({ op: 'files.rename', workspaceId, path: fileKey('main.py', attached),
      destination: fileKey('renamed.py', 'another' as IdeRootId), expectedVersion: fileVersion })).rejects.toMatchObject({ code: 'invalid-path' })
    await expect(api.request({ op: 'files.rename', workspaceId, path: fileKey('main.py', attached),
      destination: 'renamed.py', expectedVersion: fileVersion })).rejects.toMatchObject({ code: 'invalid-path' })
    expect(request).toHaveBeenCalledOnce()
  })

  it('creates unambiguous POSIX, Windows, and UNC file URIs with literal reserved characters', () => {
    expect(sourceUri(workspace, fileKey('full #%.py', attached))).toBe('file:///shared%20tools/full%20%23%25.py')
    expect(keyFromAbsolute(workspace, '/shared tools/full #%.py')).toBe(fileKey('full #%.py', attached))
    const windows = { workspaceId, path: 'C:\\Project Folder', title: 'Windows' }
    expect(sourceUri(windows, 'full #%.py')).toBe('file:///C:/Project%20Folder/full%20%23%25.py')
    expect(keyFromAbsolute(windows, 'c:\\project folder\\main.py')).toBe('main.py')
    expect(sourceUri({ ...windows, path: '\\\\server\\share' }, 'main.py')).toBe('file://server/share/main.py')
    expect(fileReference('src/main.py')).toEqual({ path: 'src/main.py' })
    expect(sourceUri(workspace, fileKey('draft.py', 'removed' as IdeRootId)))
      .toBe('untitled:rainy-recovery/project/%00removed%2Fdraft.py')
  })
})
