/** Chat history list shown in the left pane's History mode. */
import type { SessionSummary } from '../../shared/rpc.ts'

/** Inputs from the window layout. */
export interface HistoryProps {
  currentSessionId: string | null
  /** Open a chat; the layout switches to the chat's project when it differs. */
  onSelect(session: SessionSummary): void
  /** Start a new chat in the current project. */
  onNewChat(): void
}

/**
 * Placeholder until the chat UI lands.
 * @param props List inputs.
 * @returns The list.
 */
export function History(props: HistoryProps): JSX.Element {
  return <div data-chat-history data-session={props.currentSessionId ?? ''} />
}
