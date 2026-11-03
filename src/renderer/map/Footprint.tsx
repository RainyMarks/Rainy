/**
 * The AI footprint: turn by turn, what the chat's agent read, changed, searched and ran, read from the chat itself so
 * it covers turns that ran before the map was opened, and followed live while a turn runs.
 */
import { useEffect, useState } from 'react'
import type { IdeWorkspace } from '../../shared/ide-files-protocol.ts'
import type { FootprintStep, FootprintTurn } from '../../shared/map-protocol.ts'
import type { SessionId } from '../../shared/rpc.ts'
import { keyFromAbsolute } from '../ide/ide-paths.ts'
import { host } from '../rpc.ts'
import { Button } from '../ui/index.ts'
import type { MapMessageKey, useMapT } from './messages.ts'
import { Inline } from './Tour.tsx'
import css from './Tour.module.css'

type Translate = ReturnType<typeof useMapT>

/** Tool events in a row lead to one reload. */
const RELOAD_DELAY_MS = 400

const VERB: Record<FootprintStep['action'], MapMessageKey> = {
  read: 'actionRead', create: 'actionCreate', edit: 'actionEdit', delete: 'actionDelete', search: 'actionSearch',
  command: 'actionCommand', web: 'actionWeb', agent: 'actionAgent', other: 'actionOther',
}

const CHANGES = new Set<FootprintStep['action']>(['create', 'edit', 'delete'])

/** @returns The chat's turns, kept current while its agent works; `undefined` until loaded. */
function useFootprint(sessionId: SessionId | null): { turns?: readonly FootprintTurn[] | undefined; error?: string | undefined } {
  const [state, setState] = useState<{ turns?: readonly FootprintTurn[] | undefined; error?: string | undefined }>({})
  useEffect(() => {
    setState({})
    if (sessionId === null) return undefined
    let cancelled = false
    let timer: ReturnType<typeof setTimeout> | undefined
    const load = (): void => {
      host.call('map.footprint', { sessionId })
        .then(({ turns }) => { if (!cancelled) setState({ turns }) })
        .catch((error: unknown) => { if (!cancelled) setState({ error: error instanceof Error ? error.message : String(error) }) })
    }
    const later = (): void => { clearTimeout(timer); timer = setTimeout(load, RELOAD_DELAY_MS) }
    load()
    const offTool = host.on('session.tool', (event) => { if (event.sessionId === sessionId && event.phase !== 'update') later() })
    const offState = host.on('session.state', (event) => { if (event.sessionId === sessionId) later() })
    return () => { cancelled = true; clearTimeout(timer); offTool(); offState() }
  }, [sessionId])
  return state
}

/**
 * The footprint of the chat shown beside the map.
 * @param props Project, chat and actions.
 * @returns The footprint.
 */
export function FootprintView({ workspace, sessionId, onOpenFile, t }: {
  workspace: IdeWorkspace
  sessionId: SessionId | null
  onOpenFile: (key: string) => void
  t: Translate
}): JSX.Element {
  const { turns, error } = useFootprint(sessionId)
  /** The turn shown, counted from the newest; 0 follows the newest as turns come. */
  const [back, setBack] = useState(0)
  useEffect(() => { setBack(0) }, [sessionId])
  const message = (text: string): JSX.Element => <div className={css.root} data-map-footprint><div className={css.invite}><p>{text}</p></div></div>
  if (sessionId === null) return message(t('footprintNoChat'))
  if (error !== undefined) return message(t('footprintFailed', { message: error }))
  if (turns === undefined) return message(t('footprintLoading'))
  if (turns.length === 0) return message(t('footprintEmpty'))
  const index = Math.max(0, turns.length - 1 - Math.min(back, turns.length - 1))
  const turn = turns[index]!
  const key = (step: FootprintStep): string | undefined => step.path === undefined ? undefined : keyFromAbsolute(workspace, step.path)
  const files = (actions: ReadonlySet<FootprintStep['action']>): string[] =>
    [...new Set(turn.steps.filter(step => actions.has(step.action) && step.ok !== false).map(step => key(step) ?? step.label))]
  const changed = files(CHANGES)
  const read = files(new Set(['read']))
  const count = (action: FootprintStep['action']): number => turn.steps.filter(step => step.action === action).length
  const counts: Array<[MapMessageKey, number]> = [
    ['footprintCountRead', read.length], ['footprintCountChanged', changed.length], ['footprintCountSearch', count('search')],
    ['footprintCountCommand', count('command')], ['footprintCountWeb', count('web')],
  ]
  return (
    <div className={css.root} data-map-footprint>
      <article className={css.article}>
        <div className={css.turnBar}>
          <Button size="sm" variant="ghost" disabled={index === 0} onClick={() => { setBack(turns.length - index) }}>{t('footprintPrevious')}</Button>
          <span>{t('footprintTurn', { index: index + 1, total: turns.length })}</span>
          <Button size="sm" variant="ghost" disabled={index === turns.length - 1} onClick={() => { setBack(turns.length - index - 2) }}>{t('footprintNext')}</Button>
        </div>
        <p className={css.request}>{turn.text}</p>
        {turn.steps.length === 0 ? <p className={css.muted}>{t('footprintNoSteps')}</p> : <>
          <div className={css.counts}>{counts.filter(([, value]) => value > 0).map(([name, value]) => <span key={name}>{t(name, { count: value })}</span>)}</div>
          {changed.length > 0 && <section>
            <h3 className={css.heading}>{t('footprintChangedFiles')}</h3>
            <div className={css.changed}>{changed.map(path => (
              <button key={path} type="button" className={css.place} title={path} onClick={() => { onOpenFile(path) }}>{path}</button>
            ))}</div>
          </section>}
          <section>
            <h3 className={css.heading}>{t('footprintSteps')}</h3>
            <ol className={css.trail}>{turn.steps.map((step, position) => {
              const file = key(step)
              const opens = file !== undefined && step.action !== 'search' && step.action !== 'delete'
              return (
                <li key={position} data-action={step.action}>
                  <span className={css.verb}>{t(VERB[step.action])}</span>
                  {opens
                    ? <button type="button" className={css.place} title={file} onClick={() => { onOpenFile(file) }}>{file}</button>
                    : <span className={css.label} title={step.label}><Inline text={step.action === 'search' ? step.label : file ?? step.label} /></span>}
                  {step.action === 'search' && file !== undefined && file !== '' && <span className={css.label}>{`· ${file}`}</span>}
                  {step.ok === undefined && <span className={css.state} data-state="running">{t('footprintRunning')}</span>}
                  {step.ok === false && <span className={css.state} data-state="failed">{t('footprintFailedStep')}</span>}
                </li>
              )
            })}</ol>
          </section>
        </>}
      </article>
    </div>
  )
}
