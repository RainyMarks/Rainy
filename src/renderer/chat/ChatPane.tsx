/** AI assistant pane: transcript, composer and model picker for the current chat. */
import { useEffect, useRef, useState } from 'react'
import type { ImageContent } from '@earendil-works/pi-ai'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import { on } from '../app/bus.ts'
import { getPrefs } from '../prefs.ts'
import { host, useConnectionState } from '../rpc.ts'
import { IconAction, IconClockOutlineRegular, IconCloseOutlineRegular, IconNewChatOutlineRegular, toast } from '../ui/index.ts'
import { loadChat, useChat } from './chat-store.ts'
import { Composer } from './Composer.tsx'
import { useChatT } from './messages.ts'
import { Transcript } from './Transcript.tsx'
import css from './ChatPane.module.css'

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

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function Hero({ hasProject }: { hasProject: boolean }): JSX.Element {
  const t = useChatT()
  return (
    <div className={css.hero}>
      <div className={css.heroTitle}>{t('heroTitle')}</div>
      <div className={css.heroCredit}>{t('heroCredit')}</div>
      {!hasProject && <div className={css.heroHint}>{t('heroNoProject')}</div>}
    </div>
  )
}

/**
 * The AI pane.
 * @param props Pane inputs.
 * @returns The pane.
 */
export function ChatPane({ workspace, sessionId, onSessionChange, onClose, onShowHistory }: ChatPaneProps): JSX.Element {
  const t = useChatT()
  const view = useChat(sessionId)
  const connection = useConnectionState()
  const [creating, setCreating] = useState(false)
  const busy = view !== undefined && (view.summary.status === 'running' || view.summary.status === 'compacting')

  /**
   * Send to the current chat. A new chat is created first when there is none, or when `forWorkspace` names a project other than the chat's.
   */
  const send = async (text: string, images: ImageContent[], mode: 'queue' | 'steer', forWorkspace?: WorkspaceId): Promise<boolean> => {
    try {
      let target = sessionId
      if (target === null || (forWorkspace !== undefined && view !== undefined && view.summary.workspaceId !== forWorkspace)) {
        const workspaceId = forWorkspace ?? workspace?.workspaceId ?? null
        setCreating(true)
        try {
          const created = await host.call('sessions.create', { workspaceId })
          await loadChat(created.id)
          target = created.id
          onSessionChange(created.id)
        } finally {
          setCreating(false)
        }
      }
      await host.call('chat.send', { sessionId: target, text, ...(images.length === 0 ? {} : { images }), mode })
      return true
    } catch (error) {
      toast(t('sendFailed', { message: message(error) }))
      return false
    }
  }
  const sendRef = useRef(send)
  sendRef.current = send
  const busyRef = useRef(busy)
  busyRef.current = busy

  useEffect(() => on('chat.send', ({ text, workspaceId }) => {
    void sendRef.current(text, [], busyRef.current ? getPrefs().busyEnter : 'queue', workspaceId)
  }), [])

  const stop = (): void => {
    if (sessionId !== null) host.call('chat.abort', { sessionId }).catch((error: unknown) => { toast(message(error)) })
  }
  const compact = (): void => {
    if (sessionId === null) return
    host.call('chat.compact', { sessionId }).then((result) => { if (result.message !== '') toast(result.message) }, (error: unknown) => { toast(message(error)) })
  }
  const newChat = (): void => { onSessionChange(null) }

  const title = view?.summary.title !== undefined && view.summary.title !== '' ? view.summary.title : sessionId === null ? t('newChat') : t('untitled')
  return (
    <div className={css.root} data-chat-pane>
      <header className={css.header}>
        <span className={css.title} title={title}>{title}</span>
        <IconAction label={t('newChat')} onClick={newChat}><IconNewChatOutlineRegular size={16} /></IconAction>
        <IconAction label={t('history')} onClick={onShowHistory}><IconClockOutlineRegular size={16} /></IconAction>
        <IconAction label={t('close')} onClick={onClose}><IconCloseOutlineRegular size={16} /></IconAction>
      </header>
      {connection === 'closed' && <div className={css.banner} role="status">{t('disconnected')}</div>}
      {sessionId === null || view === undefined
        ? <div className={css.empty}>{sessionId === null && <Hero hasProject={workspace !== null} />}</div>
        : <Transcript view={view} onFork={onSessionChange} empty={<Hero hasProject={workspace !== null} />} />}
      <Composer sessionId={sessionId} workspaceId={view?.summary.workspaceId ?? workspace?.workspaceId ?? null} busy={busy || creating}
        model={view?.model ?? null} context={view?.context ?? null} queue={view?.queue ?? []}
        onSend={(text, images, mode) => send(text, images, mode)} onStop={stop} onCompact={compact} onNewChat={newChat} />
    </div>
  )
}
