/** Bar above the editor while the agent's changes to a file are reviewed: progress, navigation, and whole-file choices. */
import { Button } from '../ui/index.ts'
import { useReviewT } from './messages.ts'
import css from './ChangesBar.module.css'

/**
 * @param props.remaining Changes left, once the editor reported them.
 * @param props.busy An operation on the review is running.
 * @param props.onPrevious Move to the previous change.
 * @param props.onNext Move to the next change.
 * @param props.onKeep Keep every change left.
 * @param props.onUndo Undo every change.
 * @param props.onClose Leave the review; unreviewed changes stay listed.
 * @returns The bar.
 */
export function ReviewBar({ remaining, busy, onPrevious, onNext, onKeep, onUndo, onClose }: {
  remaining: number | undefined
  busy: boolean
  onPrevious(): void
  onNext(): void
  onKeep(): void
  onUndo(): void
  onClose(): void
}): JSX.Element {
  const t = useReviewT()
  return (
    <div className={css.review} role="status" data-review-bar>
      <span className={css.reviewTitle}>{remaining === undefined ? t('reviewBar', { count: '…' }) : t('reviewBar', { count: remaining })}</span>
      <Button size="sm" variant="ghost" onClick={onPrevious}>{t('reviewPrevious')}</Button>
      <Button size="sm" variant="ghost" onClick={onNext}>{t('reviewNext')}</Button>
      <span className={css.reviewSpacer} />
      <Button size="sm" variant="primary" disabled={busy} onClick={onKeep}>{t('reviewKeepFile')}</Button>
      <Button size="sm" variant="outline" disabled={busy} onClick={onUndo}>{t('reviewUndoFile')}</Button>
      <Button size="sm" variant="ghost" disabled={busy} onClick={onClose}>{t('reviewClose')}</Button>
    </div>
  )
}
