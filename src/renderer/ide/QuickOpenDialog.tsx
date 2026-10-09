/** Workspace filename search; a later query supersedes an earlier one. */
import { useEffect, useRef, useState } from 'react'
import { IconCloseOutlineRegular, IconLoadingOutlineRegular, IconSearchOutlineRegular, Modal } from '../ui/index.ts'
import type { IdeModel, IdeState } from './ide-model.ts'
import { fileLabel } from './ide-paths.ts'
import { useIdeT } from './messages.ts'
import css from './Ide.module.css'

type Props = { model: IdeModel; state: IdeState }
type SearchResult = { query: string } & (
  | { phase: 'ready'; paths: readonly string[]; truncated: boolean }
  | { phase: 'error'; message: string }
)

/** Search and open a project path without starting a chat.
 * @param props Current workspace and its model.
 * @returns The keyboard-accessible quick-open dialog, or nothing while closed.
 */
export function QuickOpenDialog(props: Props) {
  return props.state.quickOpen ? <OpenQuickOpenDialog key={props.state.workspace?.workspaceId ?? ''} {...props} /> : null
}

function OpenQuickOpenDialog({ model, state }: Props) {
  const t = useIdeT()
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
    void model.searchFiles(query, controller.signal).then((found) => {
      if (controller.signal.aborted) return
      setResult({ query, phase: 'ready', ...found })
      setSelected(0)
    }).catch((reason: unknown) => {
      if (!controller.signal.aborted) setResult({ query, phase: 'error', message: reason instanceof Error ? reason.message : String(reason) })
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
    } catch (reason) {
      if (mounted.current && currentQuery.current === query) setOpenError(reason instanceof Error ? reason.message : String(reason))
    }
  }
  return <Modal open headless className={css.quickDialog} title={t('ideQuickOpen')} onClose={() => { model.quickOpen(false) }}>
    <div className={css.quickSearch}>
      <IconSearchOutlineRegular size={16} />
      <input className={css.quickInput} role="combobox" data-modal-autofocus aria-expanded="true" aria-controls="rainy-quick-files"
        aria-activedescendant={paths.length === 0 ? undefined : `rainy-quick-file-${selected}`} aria-label={t('ideQuickOpen')}
        placeholder={t('ideSearchPlaceholder')} value={query} onChange={(event) => {
          if (event.target.value === query) return
          setQuery(event.target.value); setResult(undefined); setOpenError(''); setSelected(0)
        }} onKeyDown={(event) => {
          if (event.nativeEvent.isComposing) return
          if (event.key === 'ArrowDown') { event.preventDefault(); setSelected(Math.max(0, Math.min(paths.length - 1, selected + 1))) }
          else if (event.key === 'ArrowUp') { event.preventDefault(); setSelected(Math.max(0, selected - 1)) }
          else if (event.key === 'Enter') {
            event.preventDefault()
            const path = paths[selected] ?? query.trim()
            if (ready && path !== '') void select(path)
          }
        }} />
      {pending && <span className={css.quickSpinner} role="status" aria-label={t('ideLoading')}><IconLoadingOutlineRegular size={16} /></span>}
      <button type="button" className={`${css.button} ${css.quickClose}`} aria-label={t('ideClose')}
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
