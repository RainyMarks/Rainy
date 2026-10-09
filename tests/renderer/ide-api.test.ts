// @vitest-environment happy-dom
/** IDE requests travel over the `ide` RPC method; Host refusals keep their code and conflict state. */
import { afterEach, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { IdeDebugId } from '../../src/shared/ide-execution-protocol.ts'
import { createIdeExecutionApi } from '../../src/renderer/ide/ide-execution-api.ts'
import { callIde, createIdeFilesApi, IdeRequestError } from '../../src/renderer/ide/ide-api.ts'
import { host, HostError } from './ide-host-mock.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

afterEach(() => { host.call.mockReset() })
const workspaceId = brandString<WorkspaceId>('workspace-a')

it('sends execution requests unchanged and returns their results', async () => {
  host.call.mockResolvedValueOnce([{ name: 'counter', value: '3', type: 'int', variablesReference: 0 }])
  const request = { op: 'debug.variables' as const, workspaceId, debugId: brandString<IdeDebugId>('debug-a'), variablesReference: 4 }
  await expect(createIdeExecutionApi().request(request)).resolves.toEqual([{ name: 'counter', value: '3', type: 'int', variablesReference: 0 }])
  expect(host.call).toHaveBeenCalledExactlyOnceWith('ide', request)
})

it('turns a Host refusal into an IDE error carrying its code and conflicting state', async () => {
  const currentState = { version: 1 as const, revision: 7, data: { lastSessionId: null, tabs: [], activePath: null, expandedPaths: [], buffers: [],
    layout: { sidebarWidth: 240, agentWidth: 400, bottomHeight: 240, sidebarVisible: true, agentVisible: false, bottomVisible: false, bottomTab: 'terminal' as const } } }
  host.call.mockRejectedValueOnce(new HostError({ code: 'revision-conflict', message: 'stale', data: { currentState } }))
  const failure = createIdeFilesApi().request({ op: 'state.save', workspaceId, baseRevision: 6, data: currentState.data })
  await expect(failure).rejects.toBeInstanceOf(IdeRequestError)
  await expect(failure).rejects.toMatchObject({ code: 'revision-conflict', message: 'stale', currentState })
})

it('rejects a cancelled request at once and ignores its later answer', async () => {
  const answer = Promise.withResolvers<unknown>()
  host.call.mockReturnValueOnce(answer.promise)
  const controller = new AbortController()
  const pending = callIde({ op: 'workspaces.list' }, controller.signal)
  controller.abort()
  await expect(pending).rejects.toMatchObject({ name: 'AbortError' })
  answer.resolve([])
  const already = new AbortController()
  already.abort()
  await expect(callIde({ op: 'workspaces.list' }, already.signal)).rejects.toMatchObject({ name: 'AbortError' })
  expect(host.call).toHaveBeenCalledOnce()
})
