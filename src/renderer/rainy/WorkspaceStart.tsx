/** Replace the generic right-panel landing page with the workspace file tree. */
import { useLayoutEffect } from 'react'
import type { PropsRuntime } from '@deepseek-ai/dsh-client-ui-slots'
import type {} from '@deepseek-ai/dsh-client-ui-sidebar-right/client'

/** Navigate seeded or restored guide tabs directly to files before painting.
 * @param props Framework-provided tab actions and visibility.
 * @returns No intermediate landing page.
 */
export function WorkspaceStart({ useTabInfo }: Pick<PropsRuntime<'sidebar.right.tab.guide'>, 'useTabInfo'>) {
  const { tab } = useTabInfo()
  useLayoutEffect(() => {
    if (tab.visible) tab.actions.openTab('files', { replaceTab: true })
  }, [tab.id, tab.visible, tab.actions])
  return null
}
