/** AI assistant pane: transcript, composer and model picker for the current chat. */
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'

/** Inputs from the window layout. */
export interface ChatPaneProps {
  /** Project the next new chat starts in; `null` before a folder is opened. */
  workspace: { workspaceId: WorkspaceId; path: string; title: string } | null
  /** Chat shown in the pane; `null` shows the new-chat composer. */
  sessionId: string | null
  /** Called when the pane creates, forks or switches to a chat. */
  onSessionChange(sessionId: string | null): void
  /** Close the AI pane. */
  onClose(): void
  /** Switch the left pane to the chat history. */
  onShowHistory(): void
}

/**
 * Placeholder until the chat UI lands.
 * @param props Pane inputs.
 * @returns The pane.
 */
export function ChatPane(props: ChatPaneProps): JSX.Element {
  return <div data-chat-pane data-session={props.sessionId ?? ''} />
}
