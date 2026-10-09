/** The messages of one chat, grouped into turns with folded work steps. */
import { memo, useCallback, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import clsx from 'clsx'
import type { AssistantMessage, ImageContent, ToolCall } from '@earendil-works/pi-ai'
import type { TranscriptEntry, UiPreferences } from '../../shared/rpc.ts'
import { host } from '../rpc.ts'
import { usePrefs } from '../prefs.ts'
import {
  DisclosureRow, IconBranchOutlineRegular, IconChevronDownOutlineRegular, IconCompactOutlineRegular, IconContextInjectionOutlineRegular,
  IconCopyOutlineRegular, IconInfoOutlineRegular, IconThinkOutlineRegular, IconWarningTriangleOutlineRegular, ImageLightbox, toast,
  Tooltip, writeClipboard,
} from '../ui/index.ts'
import { groupTurns, loadChat, type ChatView } from './chat-store.ts'
import { Markdown } from './Markdown.tsx'
import { useChatT } from './messages.ts'
import { ToolCard } from './ToolCard.tsx'
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
        }, (error: unknown) => { toast(error instanceof Error ? error.message : String(error)) })
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

function Thinking({ text, redacted, running }: { text: string; redacted: boolean; running: boolean }): JSX.Element {
  const t = useChatT()
  const [open, setOpen] = useState(false)
  const toggle = useCallback(() => { setOpen(value => !value) }, [])
  return (
    <DisclosureRow icon={<IconThinkOutlineRegular size={14} />} title={running ? t('thinkingRunning') : t('thinkingDone')}
      open={open} expandable={!redacted && text !== ''} onToggle={toggle} running={running} expandOnRowClick>
      <div className={css.thinking}>{redacted ? t('thinkingRedacted') : text}</div>
    </DisclosureRow>
  )
}

function ContextRow({ entry }: { entry: Entry<'context'> }): JSX.Element {
  const t = useChatT()
  const [open, setOpen] = useState(false)
  const toggle = useCallback(() => { setOpen(value => !value) }, [])
  const title = { instructions: t('contextInstructions'), memory: t('contextMemory'), reference: t('contextReference'), notice: t('contextNotice') }[entry.label]
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
  return (
    <div className={css.notice} data-level={entry.level}>
      {entry.level === 'info' ? <IconInfoOutlineRegular size={14} /> : <IconWarningTriangleOutlineRegular size={14} />}
      <span>{entry.text}</span>
    </div>
  )
}

/** Counts shown on a folded turn. */
function stepSummary(items: readonly TranscriptEntry[], t: ChatT): string {
  let reads = 0
  let changes = 0
  let commands = 0
  let others = 0
  for (const item of items) {
    if (item.kind !== 'toolResult') continue
    if (item.toolName === 'read') reads++
    else if (item.toolName === 'write' || item.toolName === 'edit') changes++
    else if (item.toolName === 'bash' || item.toolName === 'pwsh') commands++
    else others++
  }
  return [
    reads > 0 ? t('readFiles', { count: reads }) : '',
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
  /** The turn is still being produced. */
  live: boolean
  prefs: UiPreferences
  onFork(sessionId: string): void
}

/** One user message and everything the agent did for it. */
const Turn = memo(function Turn({ sessionId, user, items, results, tools, live, prefs, onFork }: TurnProps): JSX.Element {
  const t = useChatT()
  const [stepsOpen, setStepsOpen] = useState<boolean | undefined>()
  const footer = items.findLast((item): item is Entry<'turn'> => item.kind === 'turn')

  // The final answer is the last assistant reply without tool calls; everything before it is a work step.
  let answerIndex = -1
  if (!live) {
    for (let index = items.length - 1; index >= 0; index--) {
      const item = items[index]!
      if (item.kind === 'assistant') {
        if (!hasToolCalls(item.message) && item.message.stopReason !== 'error' && item.message.stopReason !== 'aborted') answerIndex = index
        break
      }
    }
  }
  const answer = answerIndex < 0 ? undefined : items[answerIndex] as Entry<'assistant'>
  const steps = items.filter((item, index) => index !== answerIndex && item.kind !== 'turn')
  const workSteps = steps.filter(item => item.kind === 'assistant' || item.kind === 'toolResult')
  const foldable = prefs.stepDetail === 'compact' && !live && workSteps.length > 0
  const open = stepsOpen ?? !foldable
  const toolsOpen = prefs.stepDetail === 'detailed'

  const renderAssistant = (entry: Entry<'assistant'>, withText: boolean, withThinking: boolean): ReactNode[] => {
    const nodes: ReactNode[] = []
    const message = entry.message
    const streaming = entry.id === STREAMING_ID
    message.content.forEach((block, index) => {
      const key = `${entry.id}:${index}`
      if (block.type === 'thinking') {
        if (withThinking && (block.thinking.trim() !== '' || block.redacted === true)) {
          const thinkingLive = streaming && index === message.content.length - 1
          nodes.push(<Thinking key={key} text={block.thinking} redacted={block.redacted === true} running={thinkingLive} />)
        }
      } else if (block.type === 'text') {
        if (withText && block.text.trim() !== '') nodes.push(<Markdown key={key} className={css.text} text={block.text} />)
      } else {
        const call: ToolCall = block
        nodes.push(<ToolCard key={key} call={call} result={results.get(call.id)} running={tools[call.id]}
          defaultOpen={toolsOpen || tools[call.id] !== undefined} />)
      }
    })
    if (message.stopReason === 'error') {
      nodes.push(
        <div key={`${entry.id}:error`} className={css.error} role="alert">
          <IconWarningTriangleOutlineRegular size={14} />
          <div><strong>{t('errorTitle')}</strong><div className={css.errorText}>{message.errorMessage ?? ''}</div></div>
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
  const answerText = answer === undefined ? '' : assistantText(answer.message)
  const lastText = answer === undefined
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
      {open && stepNodes.length > 0 && <div className={css.steps}>{stepNodes}</div>}
      {answer !== undefined && renderAssistant(answer, true, foldable ? open : true)}
      {!live && (footer !== undefined || lastText !== undefined) && (
        <div className={css.turnFooter}>
          {lastText !== undefined && <CopyAction text={answerText !== '' ? answerText : assistantText(lastText.message)} label={t('copy')} />}
          {footer !== undefined && <ForkAction sessionId={sessionId} entryId={footer.id} onFork={onFork} />}
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
    </section>
  )
})

/** Scrollable transcript of one chat. */
export function Transcript({ view, onFork, empty }: { view: ChatView; onFork(sessionId: string): void; empty?: ReactNode }): JSX.Element {
  const t = useChatT()
  const prefs = usePrefs()
  const scroller = useRef<HTMLDivElement>(null)
  const pinned = useRef(true)
  const [atBottom, setAtBottom] = useState(true)
  const [loadingOlder, setLoadingOlder] = useState(false)
  const sessionId = view.summary.id
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

  useLayoutEffect(() => {
    const element = scroller.current
    if (element !== null && pinned.current) element.scrollTop = element.scrollHeight
  })
  useLayoutEffect(() => {
    pinned.current = true
    setAtBottom(true)
  }, [sessionId])

  const onScroll = (): void => {
    const element = scroller.current
    if (element === null) return
    const bottom = element.scrollHeight - element.scrollTop - element.clientHeight < 48
    pinned.current = bottom
    setAtBottom(bottom)
  }

  const loadOlder = (): void => {
    const element = scroller.current
    const before = element === null ? 0 : element.scrollHeight - element.scrollTop
    setLoadingOlder(true)
    loadChat(sessionId, true).then(() => {
      requestAnimationFrame(() => { if (element !== null) element.scrollTop = element.scrollHeight - before })
    }, (error: unknown) => { toast(error instanceof Error ? error.message : String(error)) })
      .finally(() => { setLoadingOlder(false) })
  }

  return (
    <div className={css.root}>
      <div ref={scroller} className={css.scroller} onScroll={onScroll} data-chat-transcript>
        <div className={css.column}>
          {view.truncated && (
            <button type="button" className={css.loadOlder} disabled={loadingOlder} onClick={loadOlder}>{t('loadOlder')}</button>
          )}
          {groups.length === 0 && empty}
          {groups.map((group, index) => (
            <Turn key={group.user?.id ?? `lead-${index}`} sessionId={sessionId} user={group.user} items={group.items} results={results}
              tools={view.tools} live={running && index === groups.length - 1} prefs={prefs} onFork={onFork} />
          ))}
          {running && view.streaming === null && Object.keys(view.tools).length === 0 && (
            <div className={css.pending} aria-live="polite">
              <span className={css.pendingDot} /><span className={css.pendingDot} /><span className={css.pendingDot} />
              {view.summary.status === 'compacting' && <span className={css.pendingLabel}>{t('compactionRunning')}</span>}
            </div>
          )}
          {view.summary.status === 'error' && view.error !== undefined && (
            <div className={css.error} role="alert"><IconWarningTriangleOutlineRegular size={14} /><div className={css.errorText}>{view.error}</div></div>
          )}
        </div>
      </div>
      {!atBottom && (
        <button type="button" className={css.toBottom} onClick={() => {
          const element = scroller.current
          if (element !== null) element.scrollTo({ top: element.scrollHeight, behavior: 'smooth' })
        }}>
          <IconChevronDownOutlineRegular size={14} />{t('backToBottom')}
        </button>
      )}
    </div>
  )
}
