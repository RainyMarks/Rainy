/** Chat history list shown in the left pane's History mode. */
import { useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import type { WorkspaceId } from '../../shared/ide-files-protocol.ts'
import type { SessionSearchHit, SessionSummary } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import {
  Button, IconEllipsisOutlineRegular, IconNewChatOutlineRegular, IconPinFillRegular, IconSearchOutlineRegular, Input, Menu, Modal, StateDot,
  toast, type MenuEntry,
} from '../ui/index.ts'
import { useSummaries } from './chat-store.ts'
import { useChatT } from './messages.ts'
import css from './History.module.css'

/** Inputs from the window layout. */
export interface HistoryProps {
  currentSessionId: string | null
  /** Project open in the window; its chats are listed first. */
  workspaceId?: WorkspaceId | null | undefined
  /** Open a chat; the layout switches to the chat's project when it differs. */
  onSelect(session: SessionSummary): void
  /** Start a new chat in the current project. */
  onNewChat(): void
}

type ChatT = ReturnType<typeof useChatT>

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

/**
 * Relative time for a history row.
 * @param ms Epoch milliseconds.
 * @param now Current epoch milliseconds.
 * @param t Chat strings.
 * @returns `刚刚`, `5 分钟前`, … or a date for anything older than a week.
 */
export function relativeTime(ms: number, now: number, t: ChatT): string {
  const minutes = Math.floor((now - ms) / 60_000)
  if (minutes < 1) return t('justNow')
  if (minutes < 60) return t('minutesAgo', { count: minutes })
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return t('hoursAgo', { count: hours })
  const days = Math.floor(hours / 24)
  if (days < 7) return t('daysAgo', { count: days })
  return new Date(ms).toLocaleDateString()
}

function folderName(cwd: string): string {
  return cwd.split(/[\\/]/).filter(Boolean).at(-1) ?? cwd
}

/**
 * Group chats for the list.
 * @param chats Chats to show, newest first.
 * @param workspaceId Project open in the window.
 * @returns Non-empty groups: pinned, current project, other projects, no project.
 */
export function groupChats(chats: readonly SessionSummary[], workspaceId: WorkspaceId | null | undefined): { id: 'pinned' | 'current' | 'other' | 'none'; chats: SessionSummary[] }[] {
  const groups = { pinned: [] as SessionSummary[], current: [] as SessionSummary[], other: [] as SessionSummary[], none: [] as SessionSummary[] }
  for (const chat of chats) {
    if (chat.pinned) groups.pinned.push(chat)
    else if (chat.workspaceId === null) groups.none.push(chat)
    else if (workspaceId !== undefined && workspaceId !== null && chat.workspaceId === workspaceId) groups.current.push(chat)
    else groups.other.push(chat)
  }
  return (['pinned', 'current', 'other', 'none'] as const).map(id => ({ id, chats: groups[id] })).filter(group => group.chats.length > 0)
}

function Row({ chat, current, workspaceId, onSelect, onDelete }: {
  chat: SessionSummary
  current: boolean
  workspaceId: WorkspaceId | null | undefined
  onSelect(chat: SessionSummary): void
  onDelete(chat: SessionSummary): void
}): JSX.Element {
  const t = useChatT()
  const [menuOpen, setMenuOpen] = useState(false)
  const [renaming, setRenaming] = useState(false)
  const [draft, setDraft] = useState('')
  const title = chat.title === '' ? t('untitled') : chat.title
  const call = (promise: Promise<unknown>): void => { promise.catch((error: unknown) => { toast(message(error)) }) }

  const items: MenuEntry[] = [
    { id: 'rename', label: t('rename') },
    { id: 'pin', label: chat.pinned ? t('unpin') : t('pin') },
    { id: 'archive', label: chat.archived ? t('unarchive') : t('archive') },
    { type: 'separator', id: 'sep' },
    { id: 'delete', label: t('delete'), danger: true },
  ]
  const select = (id: string): void => {
    setMenuOpen(false)
    if (id === 'rename') { setDraft(chat.title); setRenaming(true) }
    else if (id === 'pin') call(host.call('sessions.pin', { sessionId: chat.id, pinned: !chat.pinned }))
    else if (id === 'archive') call(host.call('sessions.archive', { sessionId: chat.id, archived: !chat.archived }))
    else if (id === 'delete') onDelete(chat)
  }
  const commit = (): void => {
    setRenaming(false)
    const next = draft.trim()
    if (next !== '' && next !== chat.title) call(host.call('sessions.rename', { sessionId: chat.id, title: next }))
  }

  const showFolder = chat.workspaceId !== null && (workspaceId === undefined || workspaceId === null || chat.workspaceId !== workspaceId)
  return (
    <li className={clsx(css.row, current && css.current)} data-archived={chat.archived || undefined}>
      {renaming ? (
        <input className={css.rename} value={draft} autoFocus aria-label={t('rename')} onChange={(event) => { setDraft(event.target.value) }}
          onBlur={commit} onKeyDown={(event) => {
            if (event.key === 'Enter') { event.preventDefault(); commit() }
            if (event.key === 'Escape') { event.preventDefault(); setRenaming(false) }
          }} />
      ) : (
        <button type="button" className={css.open} aria-current={current || undefined} onClick={() => { onSelect(chat) }} title={title}>
          {chat.status === 'running' || chat.status === 'compacting' ? <StateDot state="ongoing" size={10} /> : chat.pinned ? <IconPinFillRegular size={12} className={css.pin} /> : null}
          <span className={css.title}>{title}</span>
          <span className={css.meta}>
            {showFolder && <span className={css.folder}>{folderName(chat.cwd)}</span>}
            <span>{relativeTime(chat.updatedAt, Date.now(), t)}</span>
          </span>
        </button>
      )}
      {!renaming && (
        <Menu open={menuOpen} onClose={() => { setMenuOpen(false) }} items={items} onSelect={select} align="end" portal dense
          anchor={
            <button type="button" className={css.more} aria-label={`${title} …`} aria-haspopup="menu" aria-expanded={menuOpen}
              onClick={() => { setMenuOpen(value => !value) }}>
              <IconEllipsisOutlineRegular size={14} />
            </button>
          } />
      )}
    </li>
  )
}

/**
 * The history list.
 * @param props List inputs.
 * @returns The list.
 */
export function History({ currentSessionId, workspaceId, onSelect, onNewChat }: HistoryProps): JSX.Element {
  const t = useChatT()
  const summaries = useSummaries()
  const [query, setQuery] = useState('')
  const [hits, setHits] = useState<SessionSearchHit[] | undefined>()
  const [showArchived, setShowArchived] = useState(false)
  const [deleting, setDeleting] = useState<SessionSummary | undefined>()
  const searchSeq = useRef(0)

  useEffect(() => {
    const trimmed = query.trim()
    if (trimmed === '') { setHits(undefined); return }
    const seq = ++searchSeq.current
    const timer = window.setTimeout(() => {
      host.call('sessions.search', { query: trimmed, limit: 30 }).then((result) => {
        if (seq === searchSeq.current) setHits(result)
      }, (error: unknown) => { toast(message(error)) })
    }, 200)
    return () => { window.clearTimeout(timer) }
  }, [query])

  const visible = useMemo(() => (summaries ?? []).filter(chat => showArchived ? chat.archived : !chat.archived), [summaries, showArchived])
  const groups = useMemo(() => groupChats(visible, workspaceId), [visible, workspaceId])
  const groupTitle = { pinned: t('pinned'), current: t('currentProject'), other: t('otherProjects'), none: t('noProject') }
  const archivedCount = (summaries ?? []).filter(chat => chat.archived).length

  const confirmDelete = (): void => {
    const chat = deleting
    setDeleting(undefined)
    if (chat === undefined) return
    host.call('sessions.delete', { sessionId: chat.id }).then(() => {
      if (chat.id === currentSessionId) onNewChat()
    }, (error: unknown) => { toast(message(error)) })
  }

  return (
    <div className={css.root} data-chat-history>
      <div className={css.header}>
        <Input className={css.search} icon={<IconSearchOutlineRegular size={14} />} placeholder={t('searchHistory')} aria-label={t('searchHistory')}
          value={query} onChange={(event) => { setQuery(event.target.value) }}
          onKeyDown={(event) => { if (event.key === 'Escape') setQuery('') }} />
        <Button size="sm" variant="ghost" aria-label={t('newChat')} title={t('newChat')} onClick={onNewChat}><IconNewChatOutlineRegular size={16} /></Button>
      </div>
      <div className={css.body}>
        {hits !== undefined ? (
          hits.length === 0 ? <div className={css.empty}>{t('noResults')}</div> : (
            <ul className={css.list}>
              {hits.map(hit => (
                <li key={hit.summary.id} className={clsx(css.row, hit.summary.id === currentSessionId && css.current)}>
                  <button type="button" className={clsx(css.open, css.hit)} onClick={() => { onSelect(hit.summary) }}>
                    <span className={css.title}>{hit.summary.title === '' ? t('untitled') : hit.summary.title}</span>
                    {hit.snippet !== '' && <span className={css.snippet}>{hit.snippet}</span>}
                  </button>
                </li>
              ))}
            </ul>
          )
        ) : summaries === undefined ? null : groups.length === 0 ? (
          <div className={css.empty}>{t('noChats')}</div>
        ) : groups.map(group => (
          <section key={group.id} className={css.group}>
            <h3 className={css.groupTitle}>{groupTitle[group.id]}</h3>
            <ul className={css.list}>
              {group.chats.map(chat => (
                <Row key={chat.id} chat={chat} current={chat.id === currentSessionId} workspaceId={workspaceId} onSelect={onSelect} onDelete={setDeleting} />
              ))}
            </ul>
          </section>
        ))}
      </div>
      {(archivedCount > 0 || showArchived) && hits === undefined && (
        <button type="button" className={css.footer} onClick={() => { setShowArchived(value => !value) }}>
          {showArchived ? t('hideArchived') : `${t('showArchived')} (${archivedCount})`}
        </button>
      )}
      <Modal open={deleting !== undefined} onClose={() => { setDeleting(undefined) }} title={t('delete')} closeLabel={t('close')}
        description={deleting === undefined ? undefined : t('deleteConfirm', { title: deleting.title === '' ? t('untitled') : deleting.title })}
        footer={<>
          <Button variant="outline" onClick={() => { setDeleting(undefined) }}>{t('cancel')}</Button>
          <Button variant="primary" onClick={confirmDelete}>{t('delete')}</Button>
        </>} />
    </div>
  )
}
