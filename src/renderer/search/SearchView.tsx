/** The search pane: find text in every file of the project, open a match, and replace matches on disk and in unsaved buffers. */
import { useEffect, useMemo, useRef, useState } from 'react'
import clsx from 'clsx'
import type { IdeRootId } from '../../shared/ide-files-protocol.ts'
import { replaceInText, type TextSearchFile, type TextSearchMatch, type TextSearchOptions, type TextSearchResult } from '../../shared/text-search.ts'
import type { IdeModel, IdeState } from '../ide/ide-model.ts'
import { fileKey, fileLabel } from '../ide/ide-paths.ts'
import { host } from '../rpc.ts'
import {
  Button, FileTypeIcon, IconAction, IconChevronDownOutlineRegular, IconChevronRightOutlineRegular, IconCloseOutlineRegular,
  IconEllipsisOutlineRegular, IconRefreshOutlineRegular, isComposingKey, Modal, toast,
} from '../ui/index.ts'
import { useSearchT } from './messages.ts'
import css from './SearchView.module.css'

/** Wait after typing before searching. */
const DEBOUNCE_MS = 300

/** A request to show the pane: `at` changes each time; `text` fills the search box and `replace` opens the replace box. */
export interface SearchFocus {
  readonly at: number
  readonly text?: string | undefined
  readonly replace?: boolean | undefined
}

interface Props {
  readonly model: IdeModel
  readonly state: IdeState
  readonly focus?: SearchFocus | undefined
}

type Outcome = { readonly key: string } & ({ readonly phase: 'done'; readonly result: TextSearchResult } | { readonly phase: 'error'; readonly message: string })

const fileId = (file: Pick<TextSearchFile, 'rootId' | 'path'>): string => `${file.rootId}\0${file.path}`

/**
 * The search pane.
 * @param props The IDE model and state, and the latest request to focus the pane.
 * @returns The pane.
 */
export function SearchView({ model, state, focus }: Props): JSX.Element {
  const t = useSearchT()
  const workspace = state.workspace
  const [options, setOptions] = useState<TextSearchOptions>({ pattern: '', regex: false, caseSensitive: false, wholeWord: false, include: '', exclude: '' })
  const [replacement, setReplacement] = useState('')
  const [replaceOpen, setReplaceOpen] = useState(false)
  const [filtersOpen, setFiltersOpen] = useState(false)
  const [outcome, setOutcome] = useState<Outcome>()
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(new Set())
  const [dismissed, setDismissed] = useState<ReadonlySet<string>>(new Set())
  const [confirm, setConfirm] = useState(false)
  const [replacing, setReplacing] = useState(false)
  const [run, setRun] = useState(0)
  const input = useRef<HTMLInputElement>(null)
  const replaceInput = useRef<HTMLInputElement>(null)
  const key = JSON.stringify([workspace?.workspaceId ?? null, options, run])

  useEffect(() => {
    if (focus === undefined) return
    if (focus.text !== undefined && focus.text !== '') setOptions(current => ({ ...current, pattern: focus.text! }))
    if (focus.replace === true) setReplaceOpen(true)
    requestAnimationFrame(() => {
      const target = focus.replace === true && focus.text !== undefined && focus.text !== '' ? replaceInput.current : input.current
      target?.focus()
      target?.select()
    })
  }, [focus])

  useEffect(() => {
    if (workspace === null || options.pattern === '') return
    let live = true
    const timer = setTimeout(() => {
      host.call('search.text', { workspaceId: workspace.workspaceId, ...options })
        .then((result) => { if (live) { setOutcome({ key, phase: 'done', result }); setDismissed(new Set()) } })
        .catch((error: unknown) => {
          if (live && !(error instanceof Error && /abort/iu.test(error.message))) setOutcome({ key, phase: 'error', message: error instanceof Error ? error.message : String(error) })
        })
    }, run === 0 ? DEBOUNCE_MS : 0)
    return () => { live = false; clearTimeout(timer) }
  }, [key])

  const current = outcome?.key === key ? outcome : undefined
  const searching = workspace !== null && options.pattern !== '' && current === undefined
  const files = useMemo(() => current?.phase === 'done' ? current.result.files.filter(file => !dismissed.has(fileId(file))) : [], [current, dismissed])
  const matches = files.reduce((sum, file) => sum + file.matches.length, 0)
  const set = (change: Partial<TextSearchOptions>): void => { setOptions(previous => ({ ...previous, ...change })); setRun(0) }
  const again = (): void => { setRun(value => value + 1) }

  /** Replace in the given files: unsaved buffers in the editor, the rest on disk. */
  const replace = async (targets: readonly TextSearchFile[]): Promise<void> => {
    if (workspace === null || targets.length === 0) return
    setReplacing(true)
    try {
      let count = 0
      let changed = 0
      const disk: { rootId: IdeRootId; path: string }[] = []
      const buffers = model.state.getSnapshot().buffers
      for (const file of targets) {
        const path = fileKey(file.path, file.rootId)
        const buffer = buffers[path]
        if (buffer?.dirty !== true) { disk.push({ rootId: file.rootId, path: file.path }); continue }
        const result = replaceInText(buffer.text, options, replacement)
        if (result.count === 0) continue
        model.change(path, result.text)
        count += result.count
        changed++
      }
      if (disk.length > 0) {
        const result = await host.call('search.replace', { workspaceId: workspace.workspaceId, ...options, replacement, files: disk })
        for (const file of result.files) if (file.replaced > 0) { count += file.replaced; changed++ }
        for (const failure of result.failed.slice(0, 3)) toast(t('searchReplaceFailed', { path: fileLabel(workspace, fileKey(failure.path, failure.rootId)), reason: failure.message }), { tone: 'error' })
        model.pollNow()
      }
      toast(t('searchReplaced', { count, files: changed }))
      again()
    } catch (error) {
      toast(error instanceof Error ? error.message : String(error), { tone: 'error' })
    } finally {
      setReplacing(false)
    }
  }

  const toggle = (label: string, pressed: boolean, text: string, onClick: () => void, underline = false): JSX.Element => (
    <button type="button" className={css.toggle} aria-pressed={pressed} title={label} aria-label={label} onClick={onClick}>
      <span className={clsx(underline && css.underline)}>{text}</span>
    </button>
  )

  if (workspace === null) return <div className={css.view}><p className={css.notice}>{t('searchNoWorkspace')}</p></div>

  return (
    <div className={css.view} data-search-view>
      <div className={css.form}>
        <div className={css.row}>
          <button type="button" className={css.expand} aria-expanded={replaceOpen} title={t('searchToggleReplace')} aria-label={t('searchToggleReplace')}
            onClick={() => { setReplaceOpen(open => !open) }}>
            {replaceOpen ? <IconChevronDownOutlineRegular size={14} /> : <IconChevronRightOutlineRegular size={14} />}
          </button>
          <div className={css.fields}>
            <div className={css.field}>
              <input ref={input} className={css.input} aria-label={t('searchPlaceholder')} placeholder={t('searchPlaceholder')} value={options.pattern}
                spellCheck={false} data-search-input
                onChange={(event) => { set({ pattern: event.target.value }) }}
                onKeyDown={(event) => { if (event.key === 'Enter' && !isComposingKey(event.nativeEvent)) { event.preventDefault(); again() } }} />
              <span className={css.toggles}>
                {toggle(t('searchMatchCase'), options.caseSensitive, 'Aa', () => { set({ caseSensitive: !options.caseSensitive }) })}
                {toggle(t('searchWholeWord'), options.wholeWord, 'ab', () => { set({ wholeWord: !options.wholeWord }) }, true)}
                {toggle(t('searchRegex'), options.regex, '.*', () => { set({ regex: !options.regex }) })}
              </span>
            </div>
            {replaceOpen && <div className={css.replaceRow}>
              <div className={css.field}>
                <input ref={replaceInput} className={css.input} aria-label={t('searchReplacePlaceholder')} placeholder={t('searchReplacePlaceholder')}
                  value={replacement} spellCheck={false} data-replace-input onChange={(event) => { setReplacement(event.target.value) }}
                  onKeyDown={(event) => {
                    if (event.key === 'Enter' && !isComposingKey(event.nativeEvent) && files.length > 0) { event.preventDefault(); setConfirm(true) }
                  }} />
              </div>
              <IconAction label={t('searchReplaceAll')} disabled={files.length === 0 || replacing} onClick={() => { setConfirm(true) }}>
                <span className={css.replaceAllGlyph} aria-hidden="true">⇄</span>
              </IconAction>
            </div>}
          </div>
        </div>
        <div className={css.filterToggle}>
          <IconAction label={t('searchFilters')} pressed={filtersOpen} onClick={() => { setFiltersOpen(open => !open) }}>
            <IconEllipsisOutlineRegular size={14} />
          </IconAction>
        </div>
        {filtersOpen && <div className={css.filters}>
          <label className={css.filterLabel}>{t('searchInclude')}
            <input className={css.input} value={options.include} placeholder={t('searchIncludePlaceholder')} spellCheck={false}
              onChange={(event) => { set({ include: event.target.value }) }} />
          </label>
          <label className={css.filterLabel}>{t('searchExclude')}
            <input className={css.input} value={options.exclude} placeholder={t('searchExcludePlaceholder')} spellCheck={false}
              onChange={(event) => { set({ exclude: event.target.value }) }} />
          </label>
        </div>}
      </div>
      {options.pattern !== '' && <div className={css.summary} role="status">
        <span className={css.summaryText}>
          {searching ? t('searchSearching') : current?.phase === 'error' ? '' : files.length === 0 ? t('searchNone') : t('searchSummary', { files: files.length, matches })}
        </span>
        <span className={css.summaryActions}>
          <IconAction label={t('searchRefresh')} onClick={again}><IconRefreshOutlineRegular size={14} /></IconAction>
          <IconAction label={collapsed.size > 0 ? t('searchExpandAll') : t('searchCollapseAll')} disabled={files.length === 0}
            onClick={() => { setCollapsed(collapsed.size > 0 ? new Set() : new Set(files.map(fileId))) }}>
            {collapsed.size > 0 ? <IconChevronRightOutlineRegular size={14} /> : <IconChevronDownOutlineRegular size={14} />}
          </IconAction>
          <IconAction label={t('searchClear')} onClick={() => { set({ pattern: '' }); setReplacement(''); input.current?.focus() }}>
            <IconCloseOutlineRegular size={14} />
          </IconAction>
        </span>
      </div>}
      {current?.phase === 'error' && <p className={clsx(css.notice, css.error)} role="alert">{current.message}</p>}
      {current?.phase === 'done' && current.result.truncated && <p className={css.notice}>{t('searchTruncated', { count: current.result.matches })}</p>}
      <div className={css.results} role="tree" aria-label={t('searchTitle')} data-search-results>
        {files.map((file) => {
          const id = fileId(file)
          const path = fileKey(file.path, file.rootId)
          const label = fileLabel(workspace, path)
          const slash = label.lastIndexOf('/')
          const open = !collapsed.has(id)
          return (
            <div key={id} role="treeitem" aria-expanded={open} aria-label={label} data-search-file={label}>
              <div className={css.fileRow}>
                <button type="button" className={css.fileButton} title={label}
                  onClick={() => { setCollapsed((previous) => { const next = new Set(previous); if (next.has(id)) next.delete(id); else next.add(id); return next }) }}>
                  {open ? <IconChevronDownOutlineRegular size={12} /> : <IconChevronRightOutlineRegular size={12} />}
                  <FileTypeIcon path={file.path} size={14} />
                  <span className={css.fileName}>{label.slice(slash + 1)}</span>
                  {slash > 0 && <span className={css.fileDirectory}>{label.slice(0, slash)}</span>}
                </button>
                <span className={css.rowActions}>
                  {replaceOpen && <IconAction label={t('searchReplaceFile')} disabled={replacing} onClick={() => { void replace([file]) }}>
                    <span className={css.replaceAllGlyph} aria-hidden="true">⇄</span>
                  </IconAction>}
                  <IconAction label={t('searchDismiss')} onClick={() => { setDismissed(previous => new Set([...previous, id])) }}>
                    <IconCloseOutlineRegular size={12} />
                  </IconAction>
                </span>
                <span className={css.count}>{file.matches.length}</span>
              </div>
              {open && file.matches.map((match, index) => (
                <button key={index} type="button" className={css.match} title={`${label}:${match.line}`} data-search-match
                  onClick={() => { void model.reveal(path, match.line, match.column + 1, match.length).catch((error: unknown) => { model.fail(error) }) }}>
                  <MatchPreview match={match} options={options} replacement={replaceOpen ? replacement : undefined} />
                </button>
              ))}
            </div>
          )
        })}
      </div>
      <Modal open={confirm} onClose={() => { setConfirm(false) }} title={t('searchReplaceTitle')} closeLabel={t('close')}
        description={t('searchReplaceBody', { files: files.length, matches, replacement: replacement === '' ? t('searchReplaceEmpty') : replacement })}
        footer={<>
          <Button variant="outline" onClick={() => { setConfirm(false) }}>{t('cancel')}</Button>
          <Button variant="primary" onClick={() => { setConfirm(false); void replace(files) }}>{t('searchReplaceAll')}</Button>
        </>} />
    </div>
  )
}

/** One match: the line around it, the match marked, and what replaces it when replacing. */
function MatchPreview({ match, options, replacement }: { match: TextSearchMatch; options: TextSearchOptions; replacement: string | undefined }): JSX.Element {
  const before = match.preview.slice(0, match.previewColumn).trimStart()
  const found = match.preview.slice(match.previewColumn, match.previewColumn + match.length)
  const after = match.preview.slice(match.previewColumn + match.length)
  let replaced: string | undefined
  if (replacement !== undefined) {
    try { replaced = replaceInText(found, options, replacement).text } catch (_invalid) { replaced = replacement }
  }
  return (
    <span className={css.preview}>
      <span className={css.line}>{match.line}</span>
      <span className={css.text}>
        {before.length > 30 ? `…${before.slice(-30)}` : before}
        <mark className={clsx(css.hit, replaced !== undefined && css.removed)}>{found}</mark>
        {replaced !== undefined && <ins className={css.added}>{replaced}</ins>}
        {after}
      </span>
    </span>
  )
}
