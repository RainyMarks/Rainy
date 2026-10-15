/** Allow, always allow or deny one tool call that waits for the user, with an optional instruction for the model. */
import { useEffect, useRef, useState, type KeyboardEvent } from 'react'
import type { ApprovalDecision, PendingApproval } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { Button, isComposingKey, toast } from '../ui/index.ts'
import { useChatT } from './messages.ts'
import css from './ToolCard.module.css'

/**
 * @param props.sessionId Chat of the call.
 * @param props.approval The waiting call.
 * @returns The bar.
 */
export function ApprovalBar({ sessionId, approval }: { sessionId: string; approval: PendingApproval }): JSX.Element {
  const t = useChatT()
  const allow = useRef<HTMLButtonElement>(null)
  const [reason, setReason] = useState('')
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    // Take focus only when the user is not typing somewhere, so Enter and Esc answer right away.
    const active = document.activeElement
    if (active === null || active === document.body || !active.matches('input, textarea, [contenteditable="true"]')) allow.current?.focus()
  }, [approval.toolCallId])

  const answer = (decision: ApprovalDecision): void => {
    if (busy) return
    setBusy(true)
    host.call('chat.approve', { sessionId, toolCallId: approval.toolCallId, decision, ...(decision === 'deny' && reason.trim() !== '' ? { reason: reason.trim() } : {}) })
      .catch((error: unknown) => {
        toast(t('approvalFailed', { message: error instanceof Error ? error.message : String(error) }), { tone: 'error' })
        setBusy(false)
      })
  }
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); answer('deny') }
  }

  const text = approval.reason === 'command' ? t('approvalCommand')
    : approval.reason === 'outside-write' ? t('approvalOutsideWrite', { path: approval.path ?? '' }) : t('approvalMcp')
  const rule = approval.rule
  return (
    <div className={css.approval} role="group" aria-label={t('approvalWaiting')} onKeyDown={onKeyDown} data-approval={approval.reason}>
      <div className={css.approvalText}>{text}</div>
      <div className={css.approvalActions}>
        <Button ref={allow} variant="primary" size="sm" disabled={busy} onClick={() => { answer('allow') }}>{t('approvalAllow')}</Button>
        {rule !== undefined && (
          <Button variant="outline" size="sm" disabled={busy} title={approval.reason === 'mcp' ? t('approvalAlwaysTool') : t('approvalAlwaysCommand', { rule })}
            onClick={() => { answer('always') }}>
            {t('approvalAlways', { rule: approval.reason === 'mcp' ? rule.replace(/^mcp__/u, '').replace('__', ': ') : rule })}
          </Button>
        )}
        <Button variant="ghost" size="sm" disabled={busy} onClick={() => { answer('deny') }}>{t('approvalDeny')}</Button>
        <span className={css.approvalKeys}>{t('approvalKeys')}</span>
      </div>
      <input className={css.approvalReason} value={reason} placeholder={t('approvalReason')} aria-label={t('approvalReason')} disabled={busy}
        onChange={(event) => { setReason(event.target.value) }}
        onKeyDown={(event) => { if (event.key === 'Enter' && !isComposingKey(event.nativeEvent) && reason.trim() !== '') { event.preventDefault(); answer('deny') } }} />
    </div>
  )
}
