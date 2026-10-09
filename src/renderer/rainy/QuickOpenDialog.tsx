/** Workspace filename search with cancellation when a later query replaces it. */
import { useEffect, useRef, useState } from 'react'
import { IconCloseOutlineRegular, IconLoadingOutlineRegular, IconSearchOutlineRegular, Modal } from '@deepseek-ai/dsh-client-ui-primitives'
import type { TranslateNS } from '@deepseek-ai/dsh-client-locale/client'
import type { IdeModel, IdeState } from './ide-model.ts'
import css from './IdeShell.module.css'
import { fileLabel } from './ide-paths.ts'

type Props = { model: IdeModel; state: IdeState; t: TranslateNS<'rainy'> }
type SearchResult = { query: string } & (
  | { phase: 'ready'; paths: readonly string[]; truncated: boolean }
  | { phase: 'error'; message: string }
)

/** Search and open a project path without mounting or creating a chat.
 * @param props Current workspace, search owner, and localized dialog labels.
 * @returns The keyboard-accessible quick-open dialog.
 */
export function QuickOpenDialog(props: Props) {
  return props.state.quickOpen ? <OpenQuickOpenDialog key={props.state.workspace?.workspaceId ?? ''} {...props} /> : null
}

function OpenQuickOpenDialog({ model, state, t }: Props) {
  const [query, setQuery] = useState('')
  const [result, setResult] = useState<SearchResult>()
  const [openError, setOpenError] = useState('')
  const [selected, setSelected] = useState(0)
  const list = useRef<HTMLDivElement>(null)
  const mounted = useRef(true)
  const currentQuery = useRef(query)
  currentQuery.current = query
  const current = result?.query === query ? result : undefined
  const ready = current?.phase === 'ready'
  const paths = ready ? current.paths : []
  const pending = current === undefined
  const error = current?.phase === 'error' ? current.message : openError
  useEffect(() => { mounted.current = true; return () => { mounted.current = false } }, [])
  useEffect(() => {
    const controller = new AbortController()
    void model.searchFiles(query, controller.signal).then((result) => {
      if (controller.signal.aborted) return
      setResult({ query, phase: 'ready', ...result })
      setSelected(0)
    }).catch((error: unknown) => {
      if (!controller.signal.aborted) setResult({ query, phase: 'error', message: error instanceof Error ? error.message : String(error) })
    })
    return () => { controller.abort() }
  }, [query, state.workspace?.workspaceId, model])
  useEffect(() => {
    const option = list.current?.children[selected]
    if (!(option instanceof HTMLElement) || list.current === null) return
    const top = option.offsetTop
    if (top < list.current.scrollTop) list.current.scrollTop = top
    else if (top + option.offsetHeight > list.current.scrollTop + list.current.clientHeight)
      list.current.scrollTop = top + option.offsetHeight - list.current.clientHeight
  }, [selected, paths])
  const select = async (path: string): Promise<void> => {
    try {
      await model.openFile(path)
      if (mounted.current && currentQuery.current === query) model.quickOpen(false)
    } catch (error) {
      if (mounted.current && currentQuery.current === query) setOpenError(error instanceof Error ? error.message : String(error))
    }
  }
  return <Modal open headless className={`${css.quickDialog}`} title={t('ideQuickOpen')} onClose={() => { model.quickOpen(false) }}>
    <div className={css.quickSearch}>
      <IconSearchOutlineRegular size={16} />
      <input className={css.quickInput} role="combobox" data-modal-autofocus aria-expanded="true" aria-controls="rainy-quick-files"
        aria-activedescendant={paths.length === 0 ? undefined : `rainy-quick-file-${selected}`} aria-label={t('ideQuickOpen')}
        placeholder={t('ideSearchPlaceholder')} value={query} onChange={(event) => {
          if (event.target.value === query) return
          setQuery(event.target.value); setResult(undefined); setOpenError(''); setSelected(0)
        }} onKeyDown={(event) => {
          if (event.key === 'ArrowDown') { event.preventDefault(); setSelected(Math.max(0, Math.min(paths.length - 1, selected + 1))) }
          else if (event.key === 'ArrowUp') { event.preventDefault(); setSelected(Math.max(0, selected - 1)) }
          else if (event.key === 'Enter') {
            event.preventDefault()
            const path = paths[selected] ?? query.trim()
            if (ready && path !== '') void select(path)
          }
        }} />
      {pending && <span className={css.quickSpinner} role="status" aria-label={t('ideLoading')}><IconLoadingOutlineRegular size={16} /></span>}
      <button type="button" className={`${css.button} ${css.iconButton}`} aria-label={t('ideClose')}
        onClick={() => { model.quickOpen(false) }}><IconCloseOutlineRegular size={14} /></button>
    </div>
    {error && <div className={`${css.quickNotice} ${css.error}`} role="alert">{error}</div>}
    <div ref={list} id="rainy-quick-files" role="listbox" aria-label={t('ideExplorer')} className={css.quickFiles} aria-busy={pending}>
      {paths.map((path, index) => <button key={path} type="button" id={`rainy-quick-file-${index}`} role="option"
        title={fileLabel(state.workspace, path)} aria-selected={selected === index} className={css.quickFile}
        onClick={() => { void select(path) }}>{fileLabel(state.workspace, path)}</button>)}
    </div>
    {!pending && paths.length === 0 && error === '' && <div className={css.quickNotice}>{t('ideSearchEmpty')}</div>}
    {ready && current.truncated && <div className={css.quickNotice}>{t('ideSearchTruncated')}</div>}
  </Modal>
}
