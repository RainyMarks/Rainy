/** One tool call with its result: terminal, read, search, diff, or generic input/output cards, plus approval and undo. */
import { useEffect, useMemo, useState, type ReactNode } from 'react'
import clsx from 'clsx'
import type { ToolCall } from '@earendil-works/pi-ai'
import type { PendingApproval, TranscriptEntry } from '../../shared/rpc.ts'
import { emit } from '../app/bus.ts'
import {
  DiffBlock, diffTotals, IconChevronDownOutlineRegular, JsonTree, languageForPath, ReadBlock, SearchBlock, StateDot, TerminalBlock,
  type DiffBlockLabels, type JsonTreeLabels, type ReadBlockLabels, type SearchBlockLabels, type TerminalBlockLabels,
} from '../ui/index.ts'
import { ApprovalBar } from './ApprovalBar.tsx'
import type { RunningTool } from './chat-store.ts'
import { Markdown } from './Markdown.tsx'
import { useChatT } from './messages.ts'
import { revertChanges } from './revert.ts'
import css from './ToolCard.module.css'

type ToolResult = Extract<TranscriptEntry, { kind: 'toolResult' }>

function resultText(result: ToolResult | undefined): string {
  return result === undefined ? '' : result.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')
}

function record(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {}
}

function str(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined
}

/** Labels for the shared code-card primitives. */
function useLabels() {
  const t = useChatT()
  return useMemo(() => {
    const code = { codeLabel: t('code'), wrapLabel: t('wrap'), unwrapLabel: t('unwrap') }
    const fold = {
      copy: t('copy'), copied: t('copied'), collapseAria: t('collapse'), collapse: t('collapse'),
      expandAria: (count: number) => t('expand', { count }), expand: (count: number) => t('expand', { count }),
    }
    const terminal: TerminalBlockLabels = {
      ...fold, signal: signal => t('signal', { signal }), exitCode: code => t('exitCode', { code }), noExitCode: t('noExitCode'),
      running: t('toolRunning'), failed: t('toolFailed'), done: t('toolDone'), noOutput: t('noOutput'),
    }
    const read: ReadBlockLabels = { ...code, ...fold, window: (shown, total) => t('window', { shown, total }) }
    const diff: DiffBlockLabels = { ...code, ...fold }
    const search: SearchBlockLabels = {
      ...fold, noResults: t('searchNone'),
      pathsSummary: (shown, _total, truncated) => t(truncated ? 'searchPathsTruncated' : 'searchPaths', { shown }),
      matchesSummary: (shown, _total, files, truncated) => t(truncated ? 'searchMatchesTruncated' : 'searchMatches', { shown, files }),
    }
    const json: JsonTreeLabels = {
      copyValue: t('copyValue'), copyJson: t('copyJson'), copyPath: t('copyPath'), copyPrettyJson: t('copyPrettyJson'),
      copyCompactJson: t('copyCompactJson'), copied: t('copied'), copyFailed: t('copyFailed'), collapseNode: t('collapseNode'),
      expandNode: t('expandNode'), copyButtonTitle: action => action,
    }
    return { terminal, read, diff, json, search }
  }, [t])
}

/** A link to a web page from a search result; an address that is not http(s) is shown as text. */
function WebLink({ url, title, className, children }: { url: string; title: string; className?: string | undefined; children: ReactNode }): JSX.Element {
  if (!/^https?:\/\//iu.test(url)) return <span className={className}>{children}</span>
  return <a className={className} href={url} target="_blank" rel="noopener noreferrer" title={title}>{children}</a>
}

/** @param url A result's address. @returns Its host, for results without a site name. */
function hostOf(url: string): string {
  try { return new URL(url).hostname } catch { return '' }
}

/**
 * One-line summary of a tool call.
 * @param call The call.
 * @param t Chat messages in the interface language.
 * @returns The summary.
 */
export function toolTitle(call: ToolCall, t: ReturnType<typeof useChatT>): string {
  const args = record(call.arguments)
  switch (call.name) {
    case 'bash':
    case 'pwsh': return str(args.description) ?? str(args.command) ?? call.name
    case 'read': {
      const path = str(args.file_path) ?? ''
      const offset = typeof args.offset === 'number' ? args.offset : undefined
      const limit = typeof args.limit === 'number' ? args.limit : undefined
      return limit !== undefined ? t('toolReadLines', { path, from: offset ?? 1, to: (offset ?? 1) + limit - 1 })
        : offset !== undefined ? t('toolReadFrom', { path, from: offset }) : t('toolRead', { path })
    }
    case 'write': return t('toolWrite', { path: str(args.file_path) ?? '' })
    case 'edit': return t('toolEdit', { path: str(args.file_path) ?? '' })
    case 'grep':
    case 'glob': {
      const pattern = str(args.pattern) ?? ''
      const path = str(args.path)
      if (call.name === 'grep') return path === undefined ? t('toolGrep', { pattern }) : t('toolGrepIn', { pattern, path })
      return path === undefined ? t('toolGlob', { pattern }) : t('toolGlobIn', { pattern, path })
    }
    case 'task': return t('toolTask', { description: str(args.description) ?? '' })
    case 'command_output': return t('toolCommandOutput', { id: str(args.id) ?? '' })
    case 'stop_command': return t('toolStopCommand', { id: str(args.id) ?? '' })
    case 'web_search': return t('toolWebSearch', { query: str(args.query) ?? '' })
    case 'web_fetch': return t('toolWebFetch', { url: str(args.url) ?? '' })
    default: return call.name.startsWith('mcp__') ? call.name.slice(5).replace('__', ': ') : call.name
  }
}

/**
 * Card for one tool call.
 * @param props.sessionId Chat of the call.
 * @param props.call The call as the model wrote it.
 * @param props.result Its result, once there is one.
 * @param props.running Live output while it runs.
 * @param props.defaultOpen Whether the card starts expanded.
 * @param props.approval Set while the call waits for the user.
 * @param props.reverted The user undid this change.
 * @param props.canUndo The chat is idle, so a change can be undone.
 * @returns The card.
 */
export function ToolCard({ sessionId, call, result, running, defaultOpen, approval, reverted = false, canUndo = false }: {
  sessionId: string
  call: ToolCall
  result: ToolResult | undefined
  running: RunningTool | undefined
  defaultOpen: boolean
  approval?: PendingApproval | undefined
  reverted?: boolean | undefined
  canUndo?: boolean | undefined
}): JSX.Element {
  const t = useChatT()
  const labels = useLabels()
  const [open, setOpen] = useState(defaultOpen)
  // The card appears with the reply, before its call starts running; a call that starts then opens to show its output.
  useEffect(() => { if (defaultOpen) setOpen(true) }, [defaultOpen])
  const title = toolTitle(call, t)
  const [undoing, setUndoing] = useState(false)
  const args = record(call.arguments)
  const details = record(result?.details)
  // A call waiting for approval has not started, whatever the agent's tool phase says.
  const isRunning = result === undefined && running !== undefined && approval === undefined
  const failed = result?.isError === true
  const refused = details.refused === 'user' || details.refused === 'mode' ? details.refused : undefined
  const state = approval !== undefined ? 'warning' : isRunning ? 'ongoing' : result === undefined ? 'idle' : failed ? 'error' : 'done'
  let body: ReactNode
  let suffix: ReactNode = null

  if (call.name === 'bash' || call.name === 'pwsh' || call.name === 'command_output' || call.name === 'stop_command') {
    const output = result === undefined ? running?.partial : resultText(result)
    body = (
      <TerminalBlock command={str(details.command) ?? str(args.command) ?? ''} cwd={str(details.cwd)} output={output}
        exitCode={typeof details.exitCode === 'number' ? details.exitCode : result === undefined ? undefined : null}
        signal={str(details.signal)} running={isRunning} runStateDot={approval === undefined} labels={labels.terminal} />
    )
  } else if (call.name === 'task') {
    const activity = Array.isArray(details.activity) ? details.activity as string[] : []
    const steps = typeof details.steps === 'number' ? details.steps : 0
    const stopped = details.stopped === 'steps' ? t('taskStoppedSteps') : details.stopped === 'time' ? t('taskStoppedTime') : undefined
    const latest = activity.at(-1)
    suffix = <span className={css.totals}>{isRunning && latest !== undefined ? t('taskActivity', { tool: latest }) : t('taskSteps', { steps })}</span>
    body = <>
      {stopped !== undefined && <p className={css.notice}>{stopped}</p>}
      <Markdown text={result === undefined ? running?.partial ?? '' : resultText(result)} />
    </>
  } else if (call.name === 'read' && !failed && Array.isArray(details.lines)) {
    const path = str(details.path) ?? str(args.file_path) ?? ''
    body = (
      <ReadBlock label={path} lines={details.lines as { number: number; text: string }[]} totalLines={typeof details.totalLines === 'number' ? details.totalLines : 0}
        lang={languageForPath(path)} labels={labels.read} />
    )
  } else if ((call.name === 'grep' || call.name === 'glob') && !failed && result !== undefined) {
    const truncated = details.truncated === true
    const files = Array.isArray(details.files) ? details.files as { path: string; matches: { lineNumber: number; line: string }[] }[] : undefined
    const counts = Array.isArray(details.counts) ? details.counts as { path: string; count: number }[] : undefined
    const paths = Array.isArray(details.paths) ? details.paths as string[] : counts?.map(item => `${item.path}  (${item.count})`) ?? []
    body = files !== undefined
      ? <SearchBlock kind="matches" files={files} truncated={truncated} total={files.reduce((sum, file) => sum + file.matches.length, 0)} labels={labels.search} />
      : <SearchBlock kind="paths" paths={paths} truncated={truncated} total={paths.length} labels={labels.search} />
  } else if (call.name === 'web_search' && !failed && Array.isArray(details.results)) {
    const results = details.results as { title: string; url: string; site?: string; date?: string; snippet: string }[]
    suffix = <span className={css.totals}>{t('webResults', { count: results.length })}</span>
    body = results.length === 0 ? <p className={css.notice}>{t('searchNone')}</p> : (
      <ol className={css.webResults}>
        {results.map(item => <li key={item.url}>
          <WebLink url={item.url} title={t('webOpen')}>{item.title}</WebLink>
          <span className={css.webMeta}>{[item.site ?? hostOf(item.url), item.date].filter(Boolean).join(' · ')}</span>
          {item.snippet !== '' && <span className={css.webSnippet}>{item.snippet}</span>}
        </li>)}
      </ol>
    )
  } else if (call.name === 'web_fetch' && !failed && typeof details.url === 'string') {
    suffix = <span className={css.totals}>{t('webFetched', { count: typeof details.characters === 'number' ? details.characters.toLocaleString() : 0 })}</span>
    body = <div className={css.generic}>
      <WebLink className={css.webLink} url={details.url} title={t('webOpen')}>{str(details.title) || details.url}</WebLink>
      <pre className={css.output}>{resultText(result)}</pre>
    </div>
  } else if ((call.name === 'write' || call.name === 'edit') && !failed && typeof details.after === 'string') {
    const path = str(details.path) ?? str(args.file_path) ?? ''
    const diffs = [{ path, oldText: str(details.before) ?? null, newText: details.after }]
    const totals = diffTotals(diffs)
    suffix = <span className={css.totals}><span className={css.added}>+{totals.added}</span> <span className={css.removed}>-{totals.removed}</span></span>
    body = <DiffBlock diffs={diffs} labels={labels.diff} />
  } else {
    const text = result === undefined ? running?.partial ?? '' : resultText(result)
    const images = result?.content.filter(block => block.type === 'image') ?? []
    body = (
      <div className={css.generic}>
        {Object.keys(args).length > 0 && <JsonTree data={args} label={labels.json.copyJson} labels={labels.json} expandTopLevel={false} />}
        {text !== '' && <pre className={clsx(css.output, failed && css.error)}>{text}</pre>}
        {images.map((image, index) => image.type === 'image' && <img key={index} className={css.image} src={`data:${image.mimeType};base64,${image.data}`} alt="" />)}
      </div>
    )
  }

  if (refused !== undefined) body = <p className={css.notice}>{t(refused === 'user' ? 'toolRefusedUser' : 'toolRefusedMode')}</p>

  const path = str(args.file_path)
  const undoable = details.undoable === true && !reverted && !failed
  const undo = (): void => {
    setUndoing(true)
    void revertChanges(sessionId, [call.id]).finally(() => { setUndoing(false) })
  }
  return (
    <div className={css.card} data-tool={call.name} data-state={state}>
      <button type="button" className={css.header} aria-expanded={open} onClick={() => { setOpen(value => !value) }}>
        <StateDot state={state} size={8} />
        <span className={css.title} title={title}>{title}</span>
        {approval !== undefined && <span className={css.badge} data-tone="warning">{t('approvalWaiting')}</span>}
        {refused !== undefined && <span className={css.badge}>{t('toolDenied')}</span>}
        {reverted && <span className={css.badge}>{t('undone')}</span>}
        {suffix}
        <IconChevronDownOutlineRegular size={14} className={clsx(css.chevron, open && css.chevronOpen)} />
      </button>
      {approval !== undefined && <ApprovalBar sessionId={sessionId} approval={approval} />}
      {path !== undefined && open && (
        <button type="button" className={css.path} onClick={() => { emit('editor.open', { path }) }}>{path}</button>
      )}
      {undoable && open && (
        <button type="button" className={css.undo} disabled={!canUndo || undoing} onClick={undo}>{t('undoChange')}</button>
      )}
      {(open || approval !== undefined) && <div className={css.body}>{failed && refused === undefined && call.name !== 'bash' && call.name !== 'pwsh'
        && (call.name === 'read' || call.name === 'write' || call.name === 'edit' || call.name === 'grep' || call.name === 'glob')
        ? <pre className={clsx(css.output, css.error)}>{resultText(result)}</pre> : body}</div>}
    </div>
  )
}
