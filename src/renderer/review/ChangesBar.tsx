/** The files the agent changed in the current chat, above the composer: review each in the editor, keep it, or undo it. */
import { useState } from 'react'
import clsx from 'clsx'
import type { ChatChange } from '../../shared/rpc.ts'
import { emit } from '../app/bus.ts'
import { revertChanges } from '../chat/revert.ts'
import { host } from '../rpc.ts'
import { Button, FileTypeIcon, IconChevronDownOutlineRegular, Modal, toast } from '../ui/index.ts'
import { refreshChanges, useChanges } from './changes-store.ts'
import { useReviewT } from './messages.ts'
import css from './ChangesBar.module.css'

function split(display: string): { name: string; directory: string } {
  const normalized = display.replaceAll('\\', '/')
  const slash = normalized.lastIndexOf('/')
  return slash < 0 ? { name: normalized, directory: '' } : { name: normalized.slice(slash + 1), directory: normalized.slice(0, slash) }
}

/**
 * Changed files of one chat; hidden while there are none.
 * @param props.sessionId Chat.
 * @param props.busy The chat is running, so changes cannot be undone yet.
 * @returns The bar.
 */
export function ChangesBar({ sessionId, busy }: { sessionId: string; busy: boolean }): JSX.Element | null {
  const t = useReviewT()
  const changes = useChanges(sessionId)
  const [open, setOpen] = useState(false)
  const [pending, setPending] = useState(false)
  const [confirmUndo, setConfirmUndo] = useState(false)
  if (changes.length === 0) return null
  const added = changes.reduce((sum, change) => sum + change.added, 0)
  const removed = changes.reduce((sum, change) => sum + change.removed, 0)
  const run = (operation: Promise<unknown>): void => {
    setPending(true)
    void operation.then(() => refreshChanges(sessionId))
      .catch((error: unknown) => { toast(error instanceof Error ? error.message : String(error), { tone: 'error' }) })
      .finally(() => { setPending(false) })
  }
  const keep = (list: readonly ChatChange[]): void => {
    run(host.call('chat.keep', { sessionId, toolCallIds: list.flatMap(change => change.toolCallIds) }))
  }
  const undo = (list: readonly ChatChange[]): void => {
    run(revertChanges(sessionId, list.filter(change => change.undoable).flatMap(change => change.toolCallIds)))
  }
  const review = (change: ChatChange): void => {
    emit('review.open', { sessionId, path: change.path, toolCallIds: change.toolCallIds })
  }
  return (
    <section className={css.bar} data-changes aria-label={t('changesTitle', { count: changes.length })}>
      <div className={css.header}>
        <button type="button" className={css.toggle} aria-expanded={open} title={t(open ? 'changesCollapse' : 'changesExpand')}
          onClick={() => { setOpen(value => !value); if (!open) void refreshChanges(sessionId).catch(() => undefined) }}>
          <IconChevronDownOutlineRegular size={14} className={clsx(css.chevron, open && css.chevronOpen)} />
          <span className={css.title}>{t('changesTitle', { count: changes.length })}</span>
          <span className={css.added}>+{added}</span>
          <span className={css.removed}>−{removed}</span>
        </button>
        <Button size="sm" variant="outline" disabled={pending} onClick={() => { keep(changes) }}>{t('changesKeepAll')}</Button>
        <Button size="sm" variant="outline" disabled={pending || busy || !changes.some(change => change.undoable)}
          title={busy ? t('changesBusy') : undefined} onClick={() => { setConfirmUndo(true) }}>{t('changesUndoAll')}</Button>
      </div>
      {open && (
        <ul className={css.list}>
          {changes.map((change) => {
            const { name, directory } = split(change.display)
            const reviewable = change.comparable && !change.deleted
            return (
              <li key={change.path} className={css.row} data-change={change.display}>
                <FileTypeIcon path={name} size={14} />
                <button type="button" className={css.name} title={change.display} disabled={!reviewable}
                  onClick={() => { review(change) }}>{name}</button>
                {directory !== '' && <span className={css.directory} title={change.display}>{directory}</span>}
                {change.created && <span className={css.tag}>{t('changesCreated')}</span>}
                {change.deleted && <span className={css.tag}>{t('changesDeleted')}</span>}
                <span className={css.counts} title={change.comparable ? undefined : t('changesNotComparable')}>
                  {change.comparable ? <><span className={css.added}>+{change.added}</span> <span className={css.removed}>−{change.removed}</span></> : '…'}
                </span>
                <span className={css.actions}>
                  <button type="button" className={css.action} disabled={!reviewable} title={t('changesReviewTitle')}
                    onClick={() => { review(change) }}>{t('changesReview')}</button>
                  <button type="button" className={css.action} disabled={pending} title={t('changesKeepTitle')}
                    onClick={() => { keep([change]) }}>{t('changesKeep')}</button>
                  <button type="button" className={css.action} disabled={pending || busy || !change.undoable}
                    title={busy ? t('changesBusy') : change.undoable ? t('changesUndoTitle') : t('changesNotComparable')}
                    onClick={() => { undo([change]) }}>{t('changesUndo')}</button>
                </span>
              </li>
            )
          })}
        </ul>
      )}
      <Modal open={confirmUndo} onClose={() => { setConfirmUndo(false) }} title={t('changesUndoAllTitle')} closeLabel={t('close')}
        description={t('changesUndoAllBody', { count: changes.filter(change => change.undoable).length })}
        footer={<>
          <Button variant="outline" onClick={() => { setConfirmUndo(false) }}>{t('cancel')}</Button>
          <Button variant="primary" onClick={() => { setConfirmUndo(false); undo(changes) }}>{t('changesUndoAll')}</Button>
        </>} />
    </section>
  )
}
