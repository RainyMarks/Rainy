/** Settings window: General, Models, Skills & MCP, Runtime and Memory sections. */
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'

/** Section identifiers, in navigation order. */
export type SettingsSection = 'general' | 'models' | 'extensions' | 'runtime' | 'memory'

/** Inputs from the window layout. */
export interface SettingsDialogProps {
  open: boolean
  /** Section to show when the dialog opens. */
  section?: SettingsSection | undefined
  /** Current project for the Runtime, Memory and context-budget views. */
  workspace: { workspaceId: WorkspaceId; path: string; title: string } | null
  onClose(): void
}

/**
 * Placeholder until the settings UI lands.
 * @param props Dialog inputs.
 * @returns The dialog, or nothing while closed.
 */
export function SettingsDialog(props: SettingsDialogProps): JSX.Element | null {
  return props.open ? <div data-settings-dialog data-section={props.section ?? 'general'} /> : null
}
