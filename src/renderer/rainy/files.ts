/** Redirect existing chat file references into the independently retained source editor. */
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-client-ui-sidebar-right/client'
import type { PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import { parseFileAddress } from '@deepseek-ai/dsh-util-workspace-path'
import { useEffect } from 'react'
import type { IdeModel } from './ide-model.ts'
import { keyFromAbsolute } from './ide-paths.ts'

/** Preserve resource navigation while assigning full source contents to Monaco.
 * @param ctx Rainy feature context.
 * @param editor Retained workspace editor.
 */
export function installFiles(ctx: Context, editor: IdeModel): void {
  const id = '@deepseek-ai/dsh-client-ui-rainy/text'
  ctx.effect(() => ctx.sidebarRightTabs.register({ id, kind: 'rainy-text', patterns: ['dsh-resource://file/**'], priority: 'fallback',
    canOpen: address => parseFileAddress(address)?.scope === 'session', title: address => decodeURIComponent(address.split('/').at(-1) ?? address),
  }))
  const open = async (address: string): Promise<void> => {
    const file = parseFileAddress(address)
    if (file?.scope !== 'session') return
    const session = ctx.sessions.list.getSnapshot().byId[file.sessionId as SessionId]
    const workspace = editor.state.getSnapshot().workspaces.find(entry => entry.path === session?.cwd)
    if (workspace === undefined) throw new Error(ctx.locale.bind('rainy')('ideNoWorkspace'))
    await editor.selectWorkspace(workspace)
    if (editor.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return
    const absolute = file.path.startsWith('/') || /^[A-Za-z]:[\\/]/u.test(file.path)
    const path = absolute ? keyFromAbsolute(workspace, file.path) : file.path
    if (path === undefined) throw new Error(ctx.locale.bind('rainy')('ideOutsideRoot'))
    await editor.openFile(path)
  }
  function FileBody({ useTabInfo }: PropsRuntime<'sidebar.right.pane.tab'>) {
    const { tab } = useTabInfo()
    useEffect(() => {
      if (!tab.visible) return
      void open(tab.contentId).then(() => { tab.actions.close() }).catch((error: unknown) => { editor.fail(error) })
    }, [tab.visible, tab.contentId, tab.actions])
    return null
  }
  ctx.effect(() => ctx.slots.inject('sidebar.right.pane.tab', () => ctx.slots.register({ name: 'sidebar.right.pane.tab', key: id }, FileBody)))
}
