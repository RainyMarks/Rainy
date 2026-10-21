/** The headings of the open Markdown document in the side pane, as in Typora: click one to go there; the one being read is
 *  marked, following the rich editor's scrolling or the source editor's cursor. */
import { useDeferredValue, useEffect, useMemo, useRef } from 'react'
import { idePreviewKind } from '../../shared/ide-preview.ts'
import { useStore } from '../ui/index.ts'
import type { IdeModel, IdeState } from './ide-model.ts'
import { useIdeT } from './messages.ts'
import { outlinePosition, outlineTarget } from './outline-store.ts'
import { markdownOutline } from './viewers/markdown-outline.ts'
import { chooseViewer } from './viewers/viewer-choice.ts'
import css from './Outline.module.css'

/**
 * @param props.model Workspace model, for jumping in the source editor.
 * @param props.state Its snapshot.
 * @returns The outline of the active Markdown tab.
 */
export function Outline({ model, state }: { model: IdeModel; state: IdeState }): JSX.Element {
  const t = useIdeT()
  const path = state.data.activePath
  const buffer = path === null ? undefined : state.buffers[path]
  const markdown = path !== null && buffer !== undefined && idePreviewKind(path) === 'markdown' && buffer.document.content !== null
  const text = useDeferredValue(markdown ? buffer.text : '')
  const headings = useMemo(() => markdownOutline(text), [text])
  const tab = state.data.tabs.find(entry => entry.path === path)
  const rich = markdown && chooseViewer(path, buffer.document.readOnlyReason, tab?.view, false).viewer === 'markdown-rich'
  const position = useStore(outlinePosition)
  const caret = state.selection !== undefined && state.selection.path === path ? state.selection.endLine : undefined
  const active = rich
    ? (position.path === path ? position.index : -1)
    : caret === undefined ? -1 : headings.reduce((found, heading, index) => heading.line <= caret ? index : found, -1)
  const list = useRef<HTMLDivElement>(null)
  useEffect(() => {
    list.current?.querySelector<HTMLElement>('[aria-current="location"]')?.scrollIntoView({ block: 'nearest' })
  }, [active])
  if (!markdown) return <div className={css.empty}>{t('ideOutlineNotMarkdown')}</div>
  if (headings.length === 0) return <div className={css.empty}>{t('ideOutlineEmpty')}</div>
  const top = Math.min(...headings.map(heading => heading.level))
  return <div ref={list} className={css.list} role="navigation" aria-label={t('ideOutline')} data-outline>
    {headings.map((heading, index) => (
      <button key={`${heading.line}-${index}`} type="button" className={css.item} data-level={heading.level}
        aria-current={index === active ? 'location' : undefined} title={heading.text}
        style={{ paddingInlineStart: 12 + (heading.level - top) * 14 }}
        onClick={() => {
          const target = rich ? outlineTarget(path) : undefined
          if (target !== undefined) target.reveal(index)
          else void model.reveal(path, heading.line, 1).catch((error: unknown) => { model.fail(error) })
        }}>
        {heading.text === '' ? ' ' : heading.text}
      </button>
    ))}
  </div>
}
