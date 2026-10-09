/** Stand-ins for the chat pane, history list and settings dialog, which other modules own; they expose their props to tests. */
import { useEffect, useState } from 'react'
import type { ChatPaneProps } from '../../src/renderer/chat/ChatPane.tsx'
import type { HistoryProps } from '../../src/renderer/chat/History.tsx'
import type { SettingsDialogProps } from '../../src/renderer/settings/SettingsDialog.tsx'

/** Last props and mount counts of the stand-ins. */
export const chatState: {
  mounts: number
  unmounts: number
  chat: ChatPaneProps | undefined
  history: HistoryProps | undefined
  settings: SettingsDialogProps | undefined
} = { mounts: 0, unmounts: 0, chat: undefined, history: undefined, settings: undefined }

/** Reset {@link chatState}. */
export function resetChatState(): void {
  Object.assign(chatState, { mounts: 0, unmounts: 0, chat: undefined, history: undefined, settings: undefined })
}

/** @param props Chat pane inputs. @returns A draft field and buttons calling the pane callbacks. */
export function ChatPane(props: ChatPaneProps) {
  chatState.chat = props
  const [draft, setDraft] = useState('')
  useEffect(() => {
    chatState.mounts++
    return () => { chatState.unmounts++ }
  }, [])
  return <div data-testid="chat-pane" data-session={props.sessionId ?? ''}>
    <input aria-label="chat:draft" value={draft} onChange={(event) => { setDraft(event.target.value) }} />
    <button type="button" onClick={props.onShowHistory}>chat:history</button>
    <button type="button" onClick={props.onClose}>chat:close</button>
  </div>
}

/** @param props History inputs. @returns A search field and a new-chat button. */
export function History(props: HistoryProps) {
  chatState.history = props
  return <div data-testid="history"><input aria-label="history:search" /><button type="button" onClick={props.onNewChat}>history:new</button></div>
}

/** @param props Dialog inputs. @returns A modal stand-in while open. */
export function SettingsDialog(props: SettingsDialogProps) {
  chatState.settings = props
  return props.open
    ? <div role="dialog" aria-modal="true" aria-label="settings:dialog" data-section={props.section ?? ''}>
      <button type="button" onClick={props.onClose}>settings:close</button>
    </div>
    : null
}
