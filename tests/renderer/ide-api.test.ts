// @vitest-environment happy-dom
/** IDE requests travel over the `ide` RPC method; Host refusals keep their code. */
import { afterEach, expect, it, vi } from 'vitest'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { callIde, IdeRequestError } from '../../src/renderer/ide/ide-api.ts'
import { host, HostError } from './ide-host-mock.ts'

vi.mock('../../src/renderer/rpc.ts', () => import('./ide-host-mock.ts'))

afterEach(() => { host.call.mockReset() })
const workspaceId = 'workspace-a' as WorkspaceId

it('turns a Host refusal into an IDE error carrying its code', async () => {
  host.call.mockRejectedValueOnce(new HostError({ code: 'already-exists', message: 'assets exists' }))
  const failure = callIde({ op: 'files.mkdir', workspaceId, path: 'assets' })
  await expect(failure).rejects.toBeInstanceOf(IdeRequestError)
  await expect(failure).rejects.toMatchObject({ code: 'already-exists', message: 'assets exists' })
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
