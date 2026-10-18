/** The messages of one chat, grouped into turns with folded work steps. */
import { isValidElement, memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import clsx from 'clsx'
import type { AssistantMessage, ImageContent, ToolCall } from '@earendil-works/pi-ai'
import type { PendingApproval, RetryWait, TranscriptEntry, UiPreferences } from '../../shared/rpc.ts'
import { WIDGET_TOOL } from '../../shared/widget-protocol.ts'
import { host } from '../rpc.ts'
import { usePrefs } from '../prefs.ts'
import {
  Button, DisclosureRow, IconBranchOutlineRegular, IconChevronDownOutlineRegular, IconCompactOutlineRegular, IconContextInjectionOutlineRegular,
  IconCopyOutlineRegular, IconInfoOutlineRegular, IconRefreshOutlineRegular, IconThinkOutlineRegular, IconWarningTriangleOutlineRegular, ImageLightbox,
  Modal, toast,
  Tooltip, writeClipboard,
} from '../ui/index.ts'
import { groupTurns, loadChat, type ChatView } from './chat-store.ts'
import { Markdown } from './Markdown.tsx'
import { useChatT } from './messages.ts'
import { revertChanges } from './revert.ts'
import { useScrollPin } from './scroll-pin.ts'
import { ToolCard } from './ToolCard.tsx'
import { Widget } from './Widget.tsx'
import css from './Transcript.module.css'

type Entry<K extends TranscriptEntry['kind']> = Extract<TranscriptEntry, { kind: K }>
type ChatT = ReturnType<typeof useChatT>

/** Id of the synthetic entry that carries the streaming reply. */
const STREAMING_ID = '__streaming__'

function assistantText(message: AssistantMessage): string {
  return message.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('')
}

function hasToolCalls(message: AssistantMessage): boolean {
  return message.content.some(block => block.type === 'toolCall')
}

/** Key prefix of interactive answers, which stay shown when a turn's steps fold. */
const WIDGET_KEY = 'widget:'

function isWidgetNode(node: ReactNode): boolean {
  return isValidElement(node) && typeof node.key === 'string' && node.key.startsWith(WIDGET_KEY)
}

/**
 * Whether a tool call in a streaming reply is fully written: a later block has started, or the provider dropped its
 * scratch buffer of partial JSON, which it does when the call ends.
 */
function callWritten(message: AssistantMessage, index: number): boolean {
  if (index < message.content.length - 1) return true
  const block = message.content[index] as unknown as Record<string, unknown>
  return block.partialJson === undefined && block.partialArgs === undefined
}

/**
 * Format a duration for turn footers.
 * @param ms Milliseconds.
 * @returns `12s`, `3m 4s` or `1h 2m`.
 */
export function formatDuration(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000))
  if (seconds < 60) return `${seconds}s`
  const minutes = Math.floor(seconds / 60)
  if (minutes < 60) return `${minutes}m ${seconds % 60}s`
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`
}

/**
 * Compact token count.
 * @param tokens Token count.
 * @returns `950`, `12.3k` or `1.2M`.
 */
export function formatTokens(tokens: number): string {
  if (tokens < 1000) return String(tokens)
  if (tokens < 1_000_000) return `${(tokens / 1000).toFixed(tokens < 10_000 ? 1 : 0)}k`
  return `${(tokens / 1_000_000).toFixed(1)}M`
}

function CopyAction({ text, label }: { text: string; label: string }): JSX.Element {
  const t = useChatT()
  const [copied, setCopied] = useState(false)
  return (
    <Tooltip label={copied ? t('copied') : label} side="top" portal>
      <button type="button" className={css.action} aria-label={label} onClick={() => {
        void writeClipboard(text).then((ok) => {
          if (!ok) return
          setCopied(true)
          window.setTimeout(() => { setCopied(false) }, 1200)
        })
      }}>
        <IconCopyOutlineRegular size={14} />
      </button>
    </Tooltip>
  )
}

function ForkAction({ sessionId, entryId, onFork }: { sessionId: string; entryId: string; onFork(sessionId: string): void }): JSX.Element {
  const t = useChatT()
  return (
    <Tooltip label={t('fork')} side="top" portal>
      <button type="button" className={css.action} aria-label={t('fork')} onClick={() => {
        host.call('sessions.fork', { sessionId, entryId }).then((summary) => {
          toast(t('forked'), { tone: 'success' })
          onFork(summary.id)
        }, (error: unknown) => { toast(error instanceof Error ? error.message : String(error), { tone: 'error' }) })
      }}>
        <IconBranchOutlineRegular size={14} />
      </button>
    </Tooltip>
  )
}

function Images({ images }: { images: readonly ImageContent[] }): JSX.Element {
  const t = useChatT()
  const [shown, setShown] = useState<string | undefined>()
  return (
    <div className={css.images}>
      {images.map((image, index) => {
        const src = `data:${image.mimeType};base64,${image.data}`
        return (
          <button type="button" key={index} className={css.thumb} onClick={() => { setShown(src) }}>
            <img src={src} alt="" />
          </button>
        )
      })}
      {shown !== undefined && <ImageLightbox src={shown} alt="" labels={{ dialog: t('imageDialog'), close: t('close') }} onClose={() => { setShown(undefined) }} />}
    </div>
  )
}

const REFERENCE = /@\[([^\]]*)\]\(rainy-session:[^)]+\)/g

function UserMessage({ entry }: { entry: Entry<'user'> }): JSX.Element {
  const t = useChatT()
  const parts: ReactNode[] = []
  let last = 0
  for (const match of entry.text.matchAll(REFERENCE)) {
    parts.push(entry.text.slice(last, match.index))
    parts.push(<span key={match.index} className={css.reference}>@{match[1] === '' ? t('untitled') : match[1]}</span>)
    last = match.index + match[0].length
  }
  parts.push(entry.text.slice(last))
  return (
    <div className={css.user}>
      <div className={css.userBubble}>
        {entry.images !== undefined && entry.images.length > 0 && <Images images={entry.images} />}
        {entry.text !== '' && <div className={css.userText}>{parts}</div>}
      </div>
      <div className={css.userActions}><CopyAction text={entry.text} label={t('copy')} /></div>
    </div>
  )
}

/** When the page first saw each streaming thinking block, so its counter survives the row being shown again. */
const thinkingSeen = new Map<string, number>()

/**
 * Milliseconds since a streaming block was first seen, updated every second while it streams.
 * @param key Block identity, or `undefined` when the block is not streaming.
 * @returns Elapsed time, or 0 when not streaming.
 */
function useStreamingElapsed(key: string | undefined): number {
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    if (key === undefined) return undefined
    const timer = window.setInterval(() => { setNow(Date.now()) }, 1000)
    return () => { window.clearInterval(timer) }
  }, [key])
  if (key === undefined) return 0
  let seen = thinkingSeen.get(key)
  if (seen === undefined) {
    seen = Date.now()
    thinkingSeen.set(key, seen)
    if (thinkingSeen.size > 64) thinkingSeen.delete(thinkingSeen.keys().next().value!)
  }
  return Math.max(0, now - seen)
}

function Thinking({ text, redacted, liveKey, durationMs }: {
  text: string
  redacted: boolean
  /** Set while the block streams. */
  liveKey: string | undefined
  /** How long the finished block took, when the Host recorded it. */
  durationMs: number | undefined
}): JSX.Element {
  const t = useChatT()
  const [open, setOpen] = useState(false)
  const toggle = useCallback(() => { setOpen(value => !value) }, [])
  const running = liveKey !== undefined
  const elapsed = useStreamingElapsed(liveKey)
  const title = running ? (elapsed >= 1000 ? t('thinkingRunningFor', { duration: formatDuration(elapsed) }) : t('thinkingRunning'))
    : durationMs !== undefined && durationMs >= 1000 ? t('thinkingDoneFor', { duration: formatDuration(durationMs) }) : t('thinkingDone')
  return (
    <DisclosureRow icon={<IconThinkOutlineRegular size={14} className={running ? css.drizzle : undefined} />} title={title} titleClassName={css.numeric}
      open={open} expandable={!redacted && text !== ''} onToggle={toggle} running={running} expandOnRowClick>
      <div className={css.thinking}>{redacted ? t('thinkingRedacted') : text}</div>
    </DisclosureRow>
  )
}

function ContextRow({ entry }: { entry: Entry<'context'> }): JSX.Element {
  const t = useChatT()
  const [open, setOpen] = useState(false)
  const toggle = useCallback(() => { setOpen(value => !value) }, [])
  const title = entry.reverted !== undefined ? t('revertedNotice', { count: entry.reverted.length })
    : { instructions: t('contextInstructions'), memory: t('contextMemory'), reference: t('contextReference'), notice: t('contextNotice') }[entry.label]
  return (
    <DisclosureRow icon={<IconContextInjectionOutlineRegular size={14} />} title={title} open={open} expandable onToggle={toggle} expandOnRowClick>
      <pre className={css.contextText}>{entry.text}</pre>
    </DisclosureRow>
  )
}

function CompactionRow({ entry }: { entry: Entry<'compaction'> }): JSX.Element {
  const t = useChatT()
  const prefs = usePrefs()
  const [open, setOpen] = useState(false)
  const request = entry.request
  return (
    <div className={css.compaction}>
      <button type="button" className={css.compactionLine} aria-expanded={open} onClick={() => { setOpen(value => !value) }}>
        <IconCompactOutlineRegular size={14} />
        <span>{t(entry.trigger === 'manual' ? 'compactionManual' : 'compaction', { tokens: formatTokens(entry.tokensBefore) })}</span>
        <IconChevronDownOutlineRegular size={12} className={clsx(css.chevron, open && css.chevronOpen)} />
      </button>
      {open && request !== undefined && prefs.showUsage && (
        <div className={css.compactionUsage}>
          {t(request.mode === 'shared' ? 'compactionShared' : 'compactionSeparate')}
          {' · '}
          {t('turnUsage', { input: formatTokens(request.usage.input), cache: formatTokens(request.usage.cacheRead), output: formatTokens(request.usage.output) })}
        </div>
      )}
      {open && <Markdown className={css.compactionSummary} text={entry.summary} />}
    </div>
  )
}

function NoticeRow({ entry }: { entry: Entry<'notice'> }): JSX.Element {
  const t = useChatT()
  // Notices the Host writes with a known code follow the current language; the stored text is the language at the time.
  const text = entry.code === 'overflow-retry' ? t('noticeOverflowRetry') : entry.code === 'stopped' ? t('stopped')
    : entry.code === 'auto-retry' ? t('noticeAutoRetry') : entry.text
  return (
    <div className={css.notice} data-level={entry.level}>
      {entry.level === 'info' ? <IconInfoOutlineRegular size={14} /> : <IconWarningTriangleOutlineRegular size={14} />}
      <span>{text}</span>
    </div>
  )
}

/**
 * Ask the model again after the chat's last request failed.
 * @param sessionId Chat id.
 * @param message Error text for the toast when the Host refuses.
 */
function retryChat(sessionId: string, message: (text: string) => string): void {
  host.call('chat.retry', { sessionId }).catch((error: unknown) => { toast(message(error instanceof Error ? error.message : String(error)), { tone: 'error' }) })
}

function RetryButton({ sessionId }: { sessionId: string }): JSX.Element {
  const t = useChatT()
  return (
    <Button variant="outline" size="sm" icon={<IconRefreshOutlineRegular size={14} />} className={css.retryButton}
      onClick={() => { retryChat(sessionId, message => t('retryFailed', { message })) }}>
      {t('retry')}
    </Button>
  )
}

/** Countdown while the Host waits to retry a request that failed for a passing reason. */
function RetryCountdown({ sessionId, wait }: { sessionId: string; wait: RetryWait }): JSX.Element {
  const t = useChatT()
  const [now, setNow] = useState(Date.now)
  useEffect(() => {
    const timer = window.setInterval(() => { setNow(Date.now()) }, 250)
    return () => { window.clearInterval(timer) }
  }, [wait.at])
  const seconds = Math.max(0, Math.ceil((wait.at - now) / 1000))
  return (
    <div className={css.retryWait} role="status" aria-live="polite">
      <IconRefreshOutlineRegular size={14} className={css.retrySpin} />
      <div className={css.retryBody}>
        <div className={css.numeric}>{t('retryWaiting', { seconds, attempt: wait.attempt, max: wait.max })}</div>
        {wait.reason !== '' && <div className={css.retryReason} title={wait.reason}>{wait.reason}</div>}
      </div>
      <Button variant="outline" size="sm" onClick={() => { retryChat(sessionId, message => t('retryFailed', { message })) }}>{t('retryNow')}</Button>
    </div>
  )
}

/** Counts shown on a folded turn. */
function stepSummary(items: readonly TranscriptEntry[], t: ChatT): string {
  let reads = 0
  let searches = 0
  let changes = 0
  let commands = 0
  let others = 0
  for (const item of items) {
    if (item.kind !== 'toolResult') continue
    if (item.toolName === 'read') reads++
    else if (item.toolName === 'grep' || item.toolName === 'glob') searches++
    else if (item.toolName === 'write' || item.toolName === 'edit') changes++
    else if (item.toolName === 'bash' || item.toolName === 'pwsh') commands++
    else others++
  }
  return [
    reads > 0 ? t('readFiles', { count: reads }) : '',
    searches > 0 ? t('searched', { count: searches }) : '',
    changes > 0 ? t('changedFiles', { count: changes }) : '',
    commands > 0 ? t('ranCommands', { count: commands }) : '',
    others > 0 ? t('calledTools', { count: others }) : '',
  ].filter(Boolean).join(' · ')
}

interface TurnProps {
  sessionId: string
  user: Entry<'user'> | undefined
  items: TranscriptEntry[]
  results: ReadonlyMap<string, Entry<'toolResult'>>
  tools: ChatView['tools']
  /** Tool calls waiting for the user, by call id. */
  approvals: ReadonlyMap<string, PendingApproval>
  /** Write and edit calls the user undid. */
  reverted: ReadonlySet<string>
  /** The turn is still being produced. */
  live: boolean
  /** The chat is not running, so changes can be undone. */
  idle: boolean
  /** Id of the failed reply the chat can retry, when it is in this turn. */
  retryId: string | undefined
  prefs: UiPreferences
  onFork(sessionId: string): void
}

/** One user message and everything the agent did for it. */
const Turn = memo(function Turn({ sessionId, user, items, results, tools, approvals, reverted, live, idle, retryId, prefs, onFork }: TurnProps): JSX.Element {
  const t = useChatT()
  const [stepsOpen, setStepsOpen] = useState<boolean | undefined>()
  const [confirmUndo, setConfirmUndo] = useState(false)
  const [undoing, setUndoing] = useState(false)
  const footer = items.findLast((item): item is Entry<'turn'> => item.kind === 'turn')

  // The final answer is the last assistant reply without tool calls; everything before it is a work step. A turn
  // that ended in a failed or stopped reply shows that reply in its place, so a folded turn does not hide the error.
  let answerIndex = -1
  if (!live) {
    for (let index = items.length - 1; index >= 0; index--) {
      const item = items[index]!
      if (item.kind === 'assistant') {
        const ended = item.message.stopReason === 'error' || item.message.stopReason === 'aborted'
        if (ended || !hasToolCalls(item.message)) answerIndex = index
        break
      }
    }
  }
  const answer = answerIndex < 0 ? undefined : items[answerIndex] as Entry<'assistant'>
  const failedAnswer = answer !== undefined && (answer.message.stopReason === 'error' || answer.message.stopReason === 'aborted')
  const steps = items.filter((item, index) => index !== answerIndex && item.kind !== 'turn')
  const workSteps = steps.filter(item => item.kind === 'assistant' || item.kind === 'toolResult')
  const foldable = prefs.stepDetail === 'compact' && !live && workSteps.length > 0
  const open = stepsOpen ?? !foldable
  const toolsOpen = prefs.stepDetail === 'detailed'

  // A failed reply the agent got past (a retry or a later request answered) is history, not an alert.
  const lastReply = items.findLastIndex(item => item.kind === 'assistant')
  const renderAssistant = (entry: Entry<'assistant'>, withText: boolean, withThinking: boolean): ReactNode[] => {
    const nodes: ReactNode[] = []
    const message = entry.message
    const streaming = entry.id === STREAMING_ID
    message.content.forEach((block, index) => {
      const key = `${entry.id}:${index}`
      if (block.type === 'thinking') {
        if (withThinking && (block.thinking.trim() !== '' || block.redacted === true)) {
          const thinkingLive = streaming && index === message.content.length - 1
          nodes.push(<Thinking key={key} text={block.thinking} redacted={block.redacted === true}
            liveKey={thinkingLive ? `${sessionId}:${message.timestamp}:${index}` : undefined} durationMs={entry.thinkingMs?.[index]} />)
        }
      } else if (block.type === 'text') {
        if (withText && block.text.trim() !== '') nodes.push(<Markdown key={key} className={css.text} text={block.text} />)
      } else if (block.name === WIDGET_TOOL && results.get(block.id)?.isError !== true) {
        // Keyed by the call, so the frame survives the streaming reply becoming a saved entry and the steps folding.
        const ended = message.stopReason === 'error' || message.stopReason === 'aborted'
        nodes.push(<Widget key={`${WIDGET_KEY}${block.id}`} sessionId={sessionId} call={block}
          complete={!streaming || ended || callWritten(message, index)} cut={ended && results.get(block.id) === undefined} />)
      } else {
        const call: ToolCall = block
        nodes.push(<ToolCard key={key} sessionId={sessionId} call={call} result={results.get(call.id)} running={tools[call.id]}
          defaultOpen={toolsOpen || tools[call.id] !== undefined} approval={approvals.get(call.id)} reverted={reverted.has(call.id)} canUndo={idle} />)
      }
    })
    if (message.stopReason === 'error' && items.indexOf(entry) < lastReply) {
      nodes.push(
        <div key={`${entry.id}:error`} className={css.notice} data-level="warning">
          <IconWarningTriangleOutlineRegular size={12} /><span>{t('errorInline', { message: message.errorMessage ?? '' })}</span>
        </div>,
      )
    } else if (message.stopReason === 'error') {
      nodes.push(
        <div key={`${entry.id}:error`} className={css.error} role="alert">
          <IconWarningTriangleOutlineRegular size={14} />
          <div className={css.errorBody}>
            <strong>{t('errorTitle')}</strong><div className={css.errorText}>{message.errorMessage ?? ''}</div>
            {entry.id === retryId && <RetryButton sessionId={sessionId} />}
          </div>
        </div>,
      )
    } else if (message.stopReason === 'aborted') {
      nodes.push(<div key={`${entry.id}:aborted`} className={css.stopped}>{t('stopped')}</div>)
    }
    return nodes
  }

  const stepNodes = steps.flatMap((item): ReactNode[] => {
    switch (item.kind) {
      case 'assistant': return renderAssistant(item, true, true)
      case 'toolResult': return []
      case 'context': return [<ContextRow key={item.id} entry={item} />]
      case 'compaction': return [<CompactionRow key={item.id} entry={item} />]
      case 'notice': return [<NoticeRow key={item.id} entry={item} />]
      case 'user':
      case 'turn': return []
      default: return []
    }
  })
  const widgetNodes = stepNodes.filter(isWidgetNode)
  // Changes of this turn that can still be undone, for the footer action.
  const undoable = items.flatMap(item => item.kind === 'toolResult' && !item.isError && !reverted.has(item.toolCallId)
    && item.details !== null && typeof item.details === 'object' && (item.details as { undoable?: unknown }).undoable === true ? [item.toolCallId] : [])
  const undoTurn = (): void => {
    setConfirmUndo(false)
    setUndoing(true)
    void revertChanges(sessionId, undoable).finally(() => { setUndoing(false) })
  }
  const answerText = answer === undefined || failedAnswer ? '' : assistantText(answer.message)
  const lastText = answer === undefined || failedAnswer
    ? items.findLast((item): item is Entry<'assistant'> => item.kind === 'assistant' && assistantText(item.message) !== '')
    : answer

  return (
    <section className={css.turn} data-live={live || undefined}>
      {user !== undefined && <UserMessage entry={user} />}
      {foldable && (
        <button type="button" className={css.fold} aria-expanded={open} onClick={() => { setStepsOpen(!open) }}>
          <span>{footer !== undefined ? t('workedFor', { duration: formatDuration(footer.durationMs) }) : t('stepsSummary', { steps: workSteps.length })}</span>
          {stepSummary(steps, t) !== '' && <span className={css.foldDetail}>{stepSummary(steps, t)}</span>}
          <IconChevronDownOutlineRegular size={12} className={clsx(css.chevron, open && css.chevronOpen)} />
        </button>
      )}
      {(open ? stepNodes : widgetNodes).length > 0 && <div className={css.steps}>{open ? stepNodes : widgetNodes}</div>}
      {answer !== undefined && renderAssistant(answer, true, foldable ? open : true)}
      {!live && (footer !== undefined || lastText !== undefined) && (
        <div className={css.turnFooter}>
          {lastText !== undefined && <CopyAction text={answerText !== '' ? answerText : assistantText(lastText.message)} label={t('copy')} />}
          {footer !== undefined && <ForkAction sessionId={sessionId} entryId={footer.id} onFork={onFork} />}
          {undoable.length > 0 && (
            <button type="button" className={css.undoTurn} disabled={!idle || undoing} onClick={() => { setConfirmUndo(true) }}>{t('undoTurn')}</button>
          )}
          {footer !== undefined && prefs.showUsage && (
            <span className={css.usage} title={`${footer.provider} / ${footer.model}`}>
              {!foldable && `${formatDuration(footer.durationMs)} · `}
              {t('turnUsage', {
                input: formatTokens(footer.usage.input), cache: formatTokens(footer.usage.cacheRead), output: formatTokens(footer.usage.output),
              })}
              {footer.requests > 1 && ` · ${t('turnRequests', { count: footer.requests })}`}
            </span>
          )}
        </div>
      )}
      <Modal open={confirmUndo} onClose={() => { setConfirmUndo(false) }} title={t('undoTurnTitle')} closeLabel={t('close')}
        description={t('undoTurnBody', { count: undoable.length })}
        footer={<>
          <Button variant="outline" onClick={() => { setConfirmUndo(false) }}>{t('cancel')}</Button>
          <Button variant="primary" onClick={undoTurn}>{t('undoTurn')}</Button>
        </>} />
    </section>
  )
})

/** Scrollable transcript of one chat. */
export function Transcript({ view, onFork, empty }: { view: ChatView; onFork(sessionId: string): void; empty?: ReactNode }): JSX.Element {
  const t = useChatT()
  const prefs = usePrefs()
  const [loadingOlder, setLoadingOlder] = useState(false)
  const sessionId = view.summary.id
  const pin = useScrollPin(sessionId)
  const running = view.summary.status === 'running' || view.summary.status === 'compacting'

  const entries = useMemo(() => {
    if (view.streaming === null || !running) return view.entries
    const streaming: Entry<'assistant'> = { id: STREAMING_ID, kind: 'assistant', ts: Date.now(), message: view.streaming }
    return [...view.entries, streaming]
  }, [view.entries, view.streaming, running])
  const groups = useMemo(() => groupTurns(entries), [entries])
  const results = useMemo(() => {
    const map = new Map<string, Entry<'toolResult'>>()
    for (const entry of view.entries) if (entry.kind === 'toolResult') map.set(entry.toolCallId, entry)
    return map
  }, [view.entries])
  const reverted = useMemo(() => new Set(view.entries.flatMap(entry => entry.kind === 'context' ? entry.reverted ?? [] : [])), [view.entries])
  const approvals = useMemo(() => new Map(view.approvals.map(approval => [approval.toolCallId, approval])), [view.approvals])
  // The chat can be asked again when its last model-visible entry is a failed reply.
  const retryId = useMemo(() => {
    if (running) return undefined
    const last = view.entries.findLast(entry => entry.kind === 'user' || entry.kind === 'assistant' || entry.kind === 'toolResult')
    return last?.kind === 'assistant' && last.message.stopReason === 'error' ? last.id : undefined
  }, [view.entries, running])

  // A message the user just sent is shown even when they had scrolled up to read.
  const lastUser = view.entries.findLast(entry => entry.kind === 'user')?.id
  const seenUser = useRef(lastUser)
  useLayoutEffect(() => {
    if (lastUser === seenUser.current) return
    seenUser.current = lastUser
    pin.follow()
  }, [lastUser, pin.follow])

  const loadOlder = (): void => {
    const element = pin.ref.current
    const before = element === null ? 0 : element.scrollHeight - element.scrollTop
    setLoadingOlder(true)
    loadChat(sessionId, true).then(() => {
      requestAnimationFrame(() => { if (element !== null) element.scrollTop = element.scrollHeight - before })
    }, (error: unknown) => { toast(error instanceof Error ? error.message : String(error), { tone: 'error' }) })
      .finally(() => { setLoadingOlder(false) })
  }

  return (
    <div className={css.root}>
      <div ref={pin.ref} className={css.scroller} data-chat-transcript>
        <div className={css.column}>
          {view.truncated && (
            <button type="button" className={css.loadOlder} disabled={loadingOlder} onClick={loadOlder}>{t('loadOlder')}</button>
          )}
          {groups.length === 0 && empty}
          {groups.map((group, index) => (
            <Turn key={group.user?.id ?? `lead-${index}`} sessionId={sessionId} user={group.user} items={group.items} results={results}
              tools={view.tools} approvals={approvals} reverted={reverted} live={running && index === groups.length - 1} idle={!running}
              retryId={index === groups.length - 1 ? retryId : undefined} prefs={prefs} onFork={onFork} />
          ))}
          {view.retry !== undefined && <RetryCountdown sessionId={sessionId} wait={view.retry} />}
          {running && view.retry === undefined && view.streaming === null && Object.keys(view.tools).length === 0 && (
            <div className={css.pending} aria-live="polite">
              <span className={css.pendingDot} /><span className={css.pendingDot} /><span className={css.pendingDot} />
              {view.summary.status === 'compacting' && <span className={css.pendingLabel}>{t('compactionRunning')}</span>}
            </div>
          )}
          {view.summary.status === 'error' && view.error !== undefined && (
            <div className={css.error} role="alert">
              <IconWarningTriangleOutlineRegular size={14} />
              <div className={css.errorBody}>
                <div className={css.errorText}>{view.error}</div>
                {retryId === undefined && <RetryButton sessionId={sessionId} />}
              </div>
            </div>
          )}
        </div>
      </div>
      {pin.away && (
        <button type="button" className={css.toBottom} data-live={running || undefined} onClick={() => { pin.follow(true) }}>
          <IconChevronDownOutlineRegular size={14} />{t(running ? 'backToLatest' : 'backToBottom')}
        </button>
      )}
    </div>
  )
}
