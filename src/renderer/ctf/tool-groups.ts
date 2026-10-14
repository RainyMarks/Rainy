/** Display groups match the categories authenticated by the signed tool catalog. */
import type { NativeToolSummary } from '../../shared/native-tools-protocol.ts'
import type { CtfMessageKey } from './messages.ts'

/** Display group shown as a filter and as a heading in the complete list. */
export type ToolGroup = NativeToolSummary['category']

/** Groups in display order with their localized titles. */
export const toolGroups: readonly { readonly id: ToolGroup; readonly copyKey: CtfMessageKey }[] = [
  { id: 'misc', copyKey: 'toolsGroupMisc' },
  { id: 'web', copyKey: 'toolsGroupWeb' },
  { id: 'reverse', copyKey: 'toolsGroupReverse' },
]

/**
 * Place a tool in its display group.
 * @param tool - catalog summary.
 * @returns the group used for filtering and headings.
 */
export function toolGroup(tool: Pick<NativeToolSummary, 'category'>): ToolGroup {
  return tool.category
}
