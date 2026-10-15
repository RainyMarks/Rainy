/** Terminal, diagnostics, run output, and launch-only debugger panels. */
import { useEffect, useRef, useState } from 'react'
import { Button, Choice, IconAction, IconCloseOutlineRegular, IconPlusOutlineRegular, IconSendOutlineRegular, IconStopFillRegular } from '../ui/index.ts'
import type { IdeModel, IdeState } from './ide-model.ts'
import type { IdeExecutionModel, IdeExecutionState } from './ide-execution-model.ts'
import type { EditorAppearance, EditorTerminal } from './editor-types.ts'
import { loadEditorAssets } from './editor-loader.ts'
import { fileKey, fileLabel } from './ide-paths.ts'
import { useIdeT } from './messages.ts'
import css from './Ide.module.css'

interface Props {
  readonly state: IdeState
  readonly executionState: IdeExecutionState
  readonly execution: IdeExecutionModel
  readonly model: IdeModel
  readonly reveal: (path: string, line: number, column: number) => void
  readonly appearance: EditorAppearance
  /** Send a message to the AI, for fixing a problem or explaining terminal output. */
  readonly ask?: ((text: string) => void) | undefined
}

/** Terminal output lines sent to the AI when nothing is selected. */
const SENT_LINES = 120
const SENT_CHARS = 8000

/**
 * Terminal output as plain text: escape sequences removed, and each line as it ended up after carriage returns.
 * @param output Raw output.
 * @returns The text.
 */
export function plainTerminalText(output: string): string {
  return output
    .replace(/\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)/gu, '')
    .replace(/\u001b\[[0-?]*[ -/]*[@-~]/gu, '')
    .replace(/\u001b[@-Z\\-_]/gu, '')
    .replaceAll('\r\n', '\n')
    .split('\n').map(line => line.slice(line.lastIndexOf('\r') + 1)).join('\n')
}

function TerminalPanel({ execution, snapshot, model, visible, appearance, attach }: {
  execution: IdeExecutionModel
  snapshot: IdeExecutionState
  model: IdeModel
  visible: boolean
  appearance: EditorAppearance
  attach: (terminal: EditorTerminal | undefined) => void
}) {
  const container = useRef<HTMLDivElement>(null)
  const terminal = useRef<EditorTerminal | undefined>(undefined)
  const written = useRef('')
  const [ready, setReady] = useState(0)
  useEffect(() => {
    const element = container.current
    if (element === null || snapshot.selected === null) return
    let disposed = false
    written.current = ''
    void loadEditorAssets()
      .then((assets) => {
        if (disposed) return
        terminal.current = assets.terminal(
          element,
          (data) => { void execution.input(data).catch((error: unknown) => { model.fail(error) }) },
          (cols, rows) => { void execution.resize(cols, rows).catch((error: unknown) => { model.fail(error) }) },
        )
        attach(terminal.current)
        setReady(value => value + 1)
      })
      .catch((error: unknown) => { model.fail(error) })
    return () => {
      disposed = true
      attach(undefined)
      terminal.current?.dispose()
      terminal.current = undefined
    }
  }, [execution, model, snapshot.selected])
  const output = snapshot.selected === null ? '' : (snapshot.outputs[snapshot.selected] ?? '')
  useEffect(() => {
    const view = terminal.current
    if (view === undefined) return
    if (output.startsWith(written.current)) view.write(output.slice(written.current.length))
    else {
      view.reset()
      view.write(output)
    }
    written.current = output
  }, [ready, output])
  useEffect(() => { terminal.current?.setAppearance(appearance) }, [ready, appearance])
  useEffect(() => { if (visible) terminal.current?.fit() }, [visible, ready])
  return <div ref={container} className={css.terminal} data-rainy-terminal />
}

function Variables({ reference, state, execution, run }: {
  reference: number
  state: IdeExecutionState
  execution: IdeExecutionModel
  run: (operation: Promise<unknown>) => void
}) {
  const [expanded, setExpanded] = useState<ReadonlySet<number>>(new Set())
  return (
    <ul className={css.debugList}>
      {(state.variables[reference] ?? []).map((variable, index) => (
        <li key={`${variable.name}:${index}`}>
          <button type="button" className={css.button} onClick={() => {
            if (variable.variablesReference <= 0) return
            if (expanded.has(variable.variablesReference))
              setExpanded(new Set([...expanded].filter(value => value !== variable.variablesReference)))
            else {
              setExpanded(new Set([...expanded, variable.variablesReference]))
              run(execution.expandVariables(variable.variablesReference))
            }
          }}>
            <span aria-hidden>{variable.variablesReference > 0 ? (expanded.has(variable.variablesReference) ? '▾ ' : '▸ ') : ''}</span>
            {variable.name}: {variable.value}
          </button>
          {expanded.has(variable.variablesReference) && (
            <Variables reference={variable.variablesReference} state={state} execution={execution} run={run} />
          )}
        </li>
      ))}
    </ul>
  )
}

/** Render the retained execution panels; switching tabs does not stop any process.
 * @param props Workspace source and execution snapshots plus explicit actions.
 * @returns Bottom tabs with terminal, problems, output and debugger controls.
 */
export function IdeBottom({ state, executionState, execution, model, reveal, appearance, ask }: Props) {
  const t = useIdeT()
  const terminal = useRef<EditorTerminal | undefined>(undefined)
  const [watch, setWatch] = useState('')
  const [expression, setExpression] = useState('')
  const [evaluated, setEvaluated] = useState('')
  const selectedTab = state.data.layout.bottomTab
  const run = (operation: Promise<unknown>): void => {
    void operation.catch((error: unknown) => { model.fail(error) })
  }
  const debug = executionState.status.debugSessions.find(entry => entry.id === executionState.debugId)
  const evaluationMode: 'watch' | 'repl' = debug?.language === 'c' || debug?.language === 'cpp' ? 'watch' : 'repl'
  const paused = debug?.phase === 'paused'
  const active = debug !== undefined && !['terminated', 'failed'].includes(debug.phase)
  const operations = [
    ...executionState.status.terminals.map(entry => ({ id: entry.id, label: entry.cwd })),
    ...executionState.status.runs.map(entry => ({ id: entry.id, label: entry.name })),
    ...executionState.status.debugSessions.map(entry => ({ id: entry.id, label: entry.name })),
  ]
  const addWatch = (): void => {
    const value = watch.trim()
    if (value === '') return
    const current = state.data.execution ?? { profiles: [], activeProfile: null, breakpoints: [], watches: [] }
    model.execution({ ...current, watches: [...new Set([...current.watches, value])] })
    if (paused) run(execution.evaluate(value, 'watch'))
    setWatch('')
  }
  const newTerminal = (): void => {
    run(execution.terminal())
    model.layout({ bottomTab: 'terminal' })
  }
  const tabLabel = { terminal: t('ideTerminal'), problems: t('ideProblems'), output: t('ideOutput'), debug: t('ideDebug') }
  const selectedOutput = executionState.selected === null ? '' : (executionState.outputs[executionState.selected] ?? '')
  const sendOutput = (): void => {
    const selected = selectedTab === 'terminal' ? terminal.current?.selection?.() ?? '' : ''
    const text = selected.trim() !== '' ? selected : plainTerminalText(selectedOutput).trimEnd().split('\n').slice(-SENT_LINES).join('\n').slice(-SENT_CHARS)
    if (text.trim() !== '') ask?.(t('ideTerminalAsk', { output: text }))
  }
  const fixProblem = (problem: (typeof state.problems)[number]): void => {
    const lines = state.buffers[problem.path]?.text.split('\n')
    const first = Math.max(1, problem.line - 3)
    const code = lines === undefined ? '' : lines.slice(first - 1, problem.line + 3).map((line, index) => `${first + index}: ${line}`).join('\n')
    ask?.(t('ideProblemAsk', {
      path: fileLabel(state.workspace, problem.path), line: problem.line, column: problem.column, message: problem.message,
      language: state.buffers[problem.path]?.language ?? '', code,
    }))
  }
  return (
    <>
      <div className={css.panelHeader} role="tablist" aria-label={t('ideToggleBottom')}>
        {(['terminal', 'problems', 'output', 'debug'] as const).map(tab => (
          <button key={tab} type="button" className={`${css.button} ${css.panelTab}`} role="tab" aria-selected={selectedTab === tab}
            onClick={() => { model.layout({ bottomTab: tab }) }}>
            {tabLabel[tab]}{tab === 'problems' && state.problems.length > 0 ? ` ${state.problems.length}` : ''}
          </button>
        ))}
        <span className={css.spacer} />
        <Choice
          label={t('ideTerminal')}
          value={executionState.selected ?? ''}
          items={[{ id: '', label: t('ideTerminal'), disabled: true }, ...operations.map(operation => ({ id: operation.id, label: operation.label }))]}
          onChange={(value) => {
            const operation = operations.find(entry => entry.id === value)
            if (operation !== undefined) execution.select(operation.id)
          }}
        />
        {ask !== undefined && (selectedTab === 'terminal' || selectedTab === 'output') && (
          <IconAction label={t('ideTerminalSend')} disabled={selectedOutput === ''} onClick={sendOutput}>
            <IconSendOutlineRegular size={15} />
          </IconAction>
        )}
        <IconAction label={t('ideNewTerminal')} disabled={state.workspace === null} onClick={newTerminal}>
          <IconPlusOutlineRegular size={16} />
        </IconAction>
        <IconAction label={t('ideStop')} disabled={executionState.selected === null} onClick={() => { run(execution.stop()) }}>
          <IconStopFillRegular size={14} />
        </IconAction>
        <IconAction label={t('ideClose')} onClick={() => { model.layout({ bottomVisible: false }) }}>
          <IconCloseOutlineRegular size={14} />
        </IconAction>
      </div>
      {executionState.truncated && <div className={css.notice} role="status">{t('ideTruncated')}</div>}
      <div className={css.bottomContent} hidden={selectedTab !== 'terminal'}>
        {executionState.selected === null ? (
          <div className={css.empty}>
            <span>{t('ideTerminalEmpty')}</span>
            <Button variant="outline" size="sm" disabled={state.workspace === null} onClick={() => { run(execution.terminal()) }}>
              {t('ideNewTerminal')}
            </Button>
          </div>
        ) : (
          <TerminalPanel execution={execution} snapshot={executionState} model={model} visible={selectedTab === 'terminal'} appearance={appearance}
            attach={(value) => { terminal.current = value }} />
        )}
      </div>
      <div className={css.bottomContent} hidden={selectedTab !== 'problems'}>
        {state.problems.length === 0 ? (
          <div className={css.empty}>{t('ideNoProblems')}</div>
        ) : (
          <ul className={css.problems}>
            {state.problems.map((problem, index) => (
              <li key={`${problem.path}:${problem.line}:${index}`}>
                <button type="button" className={css.button} onClick={() => { reveal(problem.path, problem.line, problem.column) }}>
                  <span className={problem.severity === 'error' ? css.error : undefined}>{problem.severity === 'error' ? '×' : '△'}</span>
                  <span>{problem.message}</span>
                  <small>{fileLabel(state.workspace, problem.path)}:{problem.line}:{problem.column}</small>
                </button>
                {ask !== undefined && (
                  <button type="button" className={`${css.button} ${css.problemFix}`} onClick={() => { fixProblem(problem) }}>{t('ideProblemFix')}</button>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className={css.bottomContent} hidden={selectedTab !== 'output'}>
        <pre className={css.output}>
          {executionState.selected === null ? t('ideNoOutput') : (executionState.outputs[executionState.selected] ?? t('ideNoOutput'))}
        </pre>
      </div>
      <div className={css.bottomContent} hidden={selectedTab !== 'debug'}>
        <div className={css.toolbar}>
          <button type="button" className={css.button} disabled={!active} onClick={() => { run(execution.control(paused ? 'continue' : 'pause')) }}>
            {t(paused ? 'ideContinue' : 'idePause')}
          </button>
          <button type="button" className={css.button} disabled={!paused || debug.capabilities?.next === false}
            onClick={() => { run(execution.control('next')) }}>{t('ideStepOver')}</button>
          <button type="button" className={css.button} disabled={!paused || debug.capabilities?.stepIn === false}
            onClick={() => { run(execution.control('stepIn')) }}>{t('ideStepInto')}</button>
          <button type="button" className={css.button} disabled={!paused || debug.capabilities?.stepOut === false}
            onClick={() => { run(execution.control('stepOut')) }}>{t('ideStepOut')}</button>
          <span className={css.spacer} />
          <span>{debug?.reason ?? debug?.name ?? t('ideNoDebug')}</span>
        </div>
        <div className={css.debugGrid}>
          <section className={css.debugColumn}>
            <h3>{t('ideStack')}</h3>
            <Choice
              label={t('ideThreads')}
              value={String(executionState.threadId ?? '')}
              items={[{ id: '', label: t('ideThreads'), disabled: true }, ...executionState.threads.map(thread => ({ id: String(thread.id), label: thread.name }))]}
              onChange={(value) => { run(execution.selectThread(Number(value))) }}
            />
            <ul className={css.debugList}>
              {executionState.frames.map(frame => (
                <li key={frame.id}>
                  <button type="button" className={css.button} data-active={executionState.frameId === frame.id || undefined} onClick={() => {
                    run(execution.selectFrame(frame.id))
                    if (frame.path !== undefined) reveal(frame.path, frame.line, frame.column)
                  }}>
                    {frame.name} {frame.path === undefined ? '' : `${frame.path}:${frame.line}`}
                  </button>
                </li>
              ))}
            </ul>
            <h3>{t('ideBreakpoints')}</h3>
            <ul className={css.debugList}>
              {(state.data.execution?.breakpoints ?? []).flatMap(source =>
                source.lines.map(line => (
                  <li key={`${source.rootId ?? 'primary'}:${source.path}:${line}`}>
                    <button type="button" className={css.button} onClick={() => { reveal(fileKey(source.path, source.rootId), line, 1) }}>
                      ● {fileLabel(state.workspace, fileKey(source.path, source.rootId))}:{line}
                    </button>
                  </li>
                )),
              )}
            </ul>
          </section>
          <section className={css.debugColumn}>
            <h3>{t('ideVariables')}</h3>
            {executionState.scopes.map(scope => (
              <div key={scope.variablesReference}>
                <button type="button" className={css.button} onClick={() => { run(execution.expandVariables(scope.variablesReference)) }}>
                  {scope.name}
                </button>
                <Variables reference={scope.variablesReference} state={executionState} execution={execution} run={run} />
              </div>
            ))}
          </section>
          <section className={css.debugColumn}>
            <h3>{t('ideWatches')}</h3>
            <input className={css.input} aria-label={t('ideAddWatch')} placeholder={t('ideAddWatch')} value={watch}
              onChange={(event) => { setWatch(event.target.value) }}
              onKeyDown={(event) => { if (event.key === 'Enter' && !event.nativeEvent.isComposing) addWatch() }} />
            <ul className={css.debugList}>
              {(state.data.execution?.watches ?? []).map((watched) => {
                const value = executionState.watches[watched]
                return (
                  <li key={watched}>
                    <button type="button" className={css.button} disabled={!paused} onClick={() => { run(execution.evaluate(watched, 'watch')) }}>
                      {watched}: {typeof value === 'string' ? value : (value?.result ?? '')}
                    </button>
                  </li>
                )
              })}
            </ul>
          </section>
        </div>
        <div className={css.toolbar}>
          <input className={css.input} aria-label={t('ideConsole')} placeholder={t('ideConsole')} value={expression}
            onChange={(event) => { setExpression(event.target.value) }} />
          <button type="button" className={css.button} disabled={!paused || expression.trim() === ''} onClick={() => {
            run(execution.evaluate(expression, evaluationMode).then((result) => { setEvaluated(result.result) }))
          }}>
            {t('ideEvaluate')}
          </button>
        </div>
        {evaluated && <pre className={css.output}>{evaluated}</pre>}
      </div>
    </>
  )
}
