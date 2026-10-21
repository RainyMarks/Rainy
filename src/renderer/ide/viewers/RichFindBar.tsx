/** Find and replace inside the rich Markdown editor (Ctrl+F, Ctrl+H), drawn over the top right of the document. */
import { useEffect, useRef, useState } from 'react'
import type { EditorView } from '@milkdown/kit/prose/view'
import { IconAction, IconChevronDownOutlineRegular, IconChevronRightOutlineRegular, IconChevronUpOutlineRegular, IconCloseOutlineRegular,
  toast } from '../../ui/index.ts'
import type * as RichPlugins from './rich-plugins.ts'
import { useViewerT } from './messages.ts'

/** Why the find bar opened: Ctrl+F, or Ctrl+H with the replace row; `seed` is the selected text, `at` tells repeated presses apart. */
export interface FindRequest { readonly replace: boolean; readonly seed: string; readonly at: number }

/**
 * Scroll the highlighted match into the middle of the view when it is outside it.
 * @param view Editor.
 * @param stage Scrolling container of the document.
 * @param plugins The editor's plugin module.
 */
export function revealMatch(view: EditorView, stage: HTMLElement | null, plugins: typeof RichPlugins): void {
  const { matches, current } = plugins.currentSearch(view.state)
  const match = matches[current]
  if (match === undefined || stage === null) return
  const coordinates = view.coordsAtPos(match.from)
  const frame = stage.getBoundingClientRect()
  if (coordinates.top < frame.top + 48 || coordinates.bottom > frame.bottom - 24) stage.scrollTop += coordinates.top - (frame.top + frame.height / 3)
}

/**
 * @param props.view The editor, once created.
 * @param props.stage The document's scrolling container.
 * @param props.plugins The editor's plugin module.
 * @param props.request The latest Ctrl+F or Ctrl+H.
 * @param props.revision Changes when the document changed, so the counts are read again.
 * @param props.onClose Closed with Esc or the close button; the editor takes the focus back.
 * @returns The find bar.
 */
export function RichFindBar({ view, stage, plugins, request, revision, onClose }: {
  view: () => EditorView | undefined
  stage: () => HTMLElement | null
  plugins: typeof RichPlugins
  request: FindRequest
  revision: number
  onClose: () => void
}): JSX.Element {
  const t = useViewerT()
  const input = useRef<HTMLInputElement>(null)
  const replaceInput = useRef<HTMLInputElement>(null)
  const [query, setQuery] = useState(request.seed)
  const [replacement, setReplacement] = useState('')
  const [caseSensitive, setCaseSensitive] = useState(false)
  const [replacing, setReplacing] = useState(request.replace)
  const [, setTick] = useState(0)
  const refresh = (): void => { setTick(value => value + 1) }

  const search = (text: string, sensitive: boolean): void => {
    const editor = view()
    if (editor === undefined) return
    plugins.setSearch(editor, text, sensitive)
    revealMatch(editor, stage(), plugins)
    refresh()
  }
  useEffect(() => {
    if (request.seed !== '') setQuery(request.seed)
    if (request.replace) setReplacing(true)
    search(request.seed !== '' ? request.seed : query, caseSensitive)
    input.current?.focus()
    input.current?.select()
  // A new Ctrl+F or Ctrl+H restarts from its own selection.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [request.at])
  useEffect(() => { refresh() }, [revision])
  // Closing removes the highlights; the owner passes new callbacks on every render, so only unmounting counts.
  const owner = useRef({ view, plugins })
  owner.current = { view, plugins }
  useEffect(() => () => {
    const editor = owner.current.view()
    if (editor !== undefined) owner.current.plugins.setSearch(editor, '', false)
  }, [])

  const editor = view()
  const state = editor === undefined ? undefined : plugins.currentSearch(editor.state)
  const total = state?.matches.length ?? 0
  const step = (delta: number): void => {
    const current = view()
    if (current === undefined) return
    plugins.stepSearch(current, delta)
    revealMatch(current, stage(), plugins)
    refresh()
  }
  const close = (): void => {
    const current = view()
    if (current !== undefined) plugins.selectCurrent(current)
    onClose()
  }
  const replaceOne = (): void => {
    const current = view()
    if (current === undefined) return
    plugins.replaceCurrent(current, replacement)
    revealMatch(current, stage(), plugins)
    refresh()
    replaceInput.current?.focus()
  }
  const replaceEvery = (): void => {
    const current = view()
    if (current === undefined) return
    const count = plugins.replaceAll(current, replacement)
    if (count > 0) toast(t('replacedCount', { count }), { tone: 'success' })
    refresh()
    // The buttons are disabled once nothing matches; the replace field keeps the focus, so Esc still closes the bar.
    replaceInput.current?.focus()
  }

  return <div className="rainy-find" role="search" data-rich-find onKeyDown={(event) => {
    const command = (event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey ? event.key.toLowerCase() : ''
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close() }
    else if (command === 'f') { event.preventDefault(); input.current?.focus(); input.current?.select() }
    else if (command === 'h') {
      event.preventDefault()
      setReplacing(true)
      requestAnimationFrame(() => { replaceInput.current?.focus(); replaceInput.current?.select() })
    }
  }}>
    <IconAction label={t('findToggleReplace')} pressed={replacing} onClick={() => { setReplacing(value => !value) }}>
      {replacing ? <IconChevronDownOutlineRegular size={14} /> : <IconChevronRightOutlineRegular size={14} />}
    </IconAction>
    <div className="rainy-find-fields">
      <div className="rainy-find-row">
        <input ref={input} className="rainy-find-input" value={query} placeholder={t('findPlaceholder')} aria-label={t('findPlaceholder')} spellCheck={false}
          onChange={(event) => { setQuery(event.target.value); search(event.target.value, caseSensitive) }}
          onKeyDown={(event) => {
            if (event.key === 'Enter' && !event.nativeEvent.isComposing) { event.preventDefault(); step(event.shiftKey ? -1 : 1) }
          }} />
        <span className="rainy-find-count" aria-live="polite">
          {query === '' ? '' : total === 0 ? t('findNone') : t('findCount', { current: (state?.current ?? 0) + 1, total })}
        </span>
        <button type="button" className="rainy-find-toggle" aria-pressed={caseSensitive} title={t('findCase')} aria-label={t('findCase')}
          onClick={() => { setCaseSensitive(!caseSensitive); search(query, !caseSensitive) }}>Aa</button>
        <IconAction label={t('findPrevious')} disabled={total === 0} onClick={() => { step(-1) }}><IconChevronUpOutlineRegular size={14} /></IconAction>
        <IconAction label={t('findNext')} disabled={total === 0} onClick={() => { step(1) }}><IconChevronDownOutlineRegular size={14} /></IconAction>
        <IconAction label={t('findClose')} onClick={close}><IconCloseOutlineRegular size={14} /></IconAction>
      </div>
      {replacing && <div className="rainy-find-row">
        <input ref={replaceInput} className="rainy-find-input" value={replacement} placeholder={t('replacePlaceholder')} aria-label={t('replacePlaceholder')} spellCheck={false}
          onChange={(event) => { setReplacement(event.target.value) }}
          onKeyDown={(event) => {
            if (event.key !== 'Enter' || event.nativeEvent.isComposing) return
            event.preventDefault()
            if (event.ctrlKey && event.altKey) replaceEvery(); else replaceOne()
          }} />
        <button type="button" className="rainy-find-button" disabled={total === 0} onClick={replaceOne}>{t('replaceOne')}</button>
        <button type="button" className="rainy-find-button" disabled={total === 0} onClick={replaceEvery}>{t('replaceAll')}</button>
      </div>}
    </div>
  </div>
}
