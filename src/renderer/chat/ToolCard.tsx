/** One tool call with its result: terminal, read, diff, or generic input/output cards. */
import { useMemo, useState, type ReactNode } from 'react'
import clsx from 'clsx'
import type { ToolCall } from '@earendil-works/pi-ai'
import type { TranscriptEntry } from '../../shared/rpc.ts'
import { emit } from '../app/bus.ts'
import {
  DiffBlock, diffTotals, IconChevronDownOutlineRegular, JsonTree, languageForPath, ReadBlock, StateDot, TerminalBlock,
  type DiffBlockLabels, type JsonTreeLabels, type ReadBlockLabels, type TerminalBlockLabels,
} from '../ui/index.ts'
import type { RunningTool } from './chat-store.ts'
import { useChatT } from './messages.ts'
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
    const json: JsonTreeLabels = {
      copyValue: t('copyValue'), copyJson: t('copyJson'), copyPath: t('copyPath'), copyPrettyJson: t('copyPrettyJson'),
      copyCompactJson: t('copyCompactJson'), copied: t('copied'), copyFailed: t('copyFailed'), collapseNode: t('collapseNode'),
      expandNode: t('expandNode'), copyButtonTitle: action => action,
    }
    return { terminal, read, diff, json }
  }, [t])
}

/** One-line summary of a tool call. */
export function toolTitle(call: ToolCall): string {
  const args = record(call.arguments)
  switch (call.name) {
    case 'bash':
    case 'pwsh': return str(args.description) ?? str(args.command) ?? call.name
    case 'read': {
      const path = str(args.file_path) ?? ''
      const offset = typeof args.offset === 'number' ? args.offset : undefined
      const limit = typeof args.limit === 'number' ? args.limit : undefined
      return `Read ${path}${limit !== undefined ? ` (${offset ?? 1} - ${(offset ?? 1) + limit - 1})` : offset !== undefined ? ` (from line ${offset})` : ''}`
    }
    case 'write': return `Write ${str(args.file_path) ?? ''}`
    case 'edit': return `Edit ${str(args.file_path) ?? ''}`
    default: return call.name.startsWith('mcp__') ? call.name.slice(5).replace('__', ': ') : call.name
  }
}

/** Card for one tool call. */
export function ToolCard({ call, result, running, defaultOpen }: {
  call: ToolCall
  result: ToolResult | undefined
  running: RunningTool | undefined
  defaultOpen: boolean
}): JSX.Element {
  const labels = useLabels()
  const [open, setOpen] = useState(defaultOpen)
  const args = record(call.arguments)
  const details = record(result?.details)
  const isRunning = result === undefined && running !== undefined
  const failed = result?.isError === true
  const state = isRunning ? 'ongoing' : result === undefined ? 'idle' : failed ? 'error' : 'done'
  let body: ReactNode
  let suffix: ReactNode = null

  if (call.name === 'bash' || call.name === 'pwsh') {
    const output = result === undefined ? running?.partial : resultText(result)
    body = (
      <TerminalBlock command={str(args.command) ?? ''} cwd={str(details.cwd)} output={output}
        exitCode={typeof details.exitCode === 'number' ? details.exitCode : result === undefined ? undefined : null}
        signal={str(details.signal)} running={isRunning} labels={labels.terminal} />
    )
  } else if (call.name === 'read' && !failed && Array.isArray(details.lines)) {
    const path = str(details.path) ?? str(args.file_path) ?? ''
    body = (
      <ReadBlock label={path} lines={details.lines as { number: number; text: string }[]} totalLines={typeof details.totalLines === 'number' ? details.totalLines : 0}
        lang={languageForPath(path)} labels={labels.read} />
    )
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

  const path = str(args.file_path)
  return (
    <div className={css.card} data-tool={call.name} data-state={state}>
      <button type="button" className={css.header} aria-expanded={open} onClick={() => { setOpen(value => !value) }}>
        <StateDot state={state} size={8} />
        <span className={css.title} title={toolTitle(call)}>{toolTitle(call)}</span>
        {suffix}
        <IconChevronDownOutlineRegular size={14} className={clsx(css.chevron, open && css.chevronOpen)} />
      </button>
      {path !== undefined && open && (
        <button type="button" className={css.path} onClick={() => { emit('editor.open', { path }) }}>{path}</button>
      )}
      {open && <div className={css.body}>{failed && call.name !== 'bash' && call.name !== 'pwsh' && (call.name === 'read' || call.name === 'write' || call.name === 'edit')
        ? <pre className={clsx(css.output, css.error)}>{resultText(result)}</pre> : body}</div>}
    </div>
  )
}
