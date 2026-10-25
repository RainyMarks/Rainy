/** Secondary toolbar actions of the viewers, which fold into a "更多" menu when the page is narrow. */
import { useEffect, useState } from 'react'
import { IconEllipsisOutlineRegular, Menu, MenuItemButton } from '../../ui/index.ts'
import { useViewerT } from './messages.ts'
import css from './Viewers.module.css'

/** Page width below which secondary actions fold into the menu. Each viewer page fills its own WebView2 pane, so the
 *  page width is the pane width. */
export const NARROW_TOOLBAR = 520

/** One toolbar action that can be shown as a button or as a menu row. */
export interface ToolAction {
  readonly id: string
  readonly label: string
  readonly title?: string | undefined
  readonly disabled?: boolean | undefined
  readonly onSelect: () => void
}

/** @returns Whether the page is narrower than `width`, kept current. */
export function useNarrow(width = NARROW_TOOLBAR): boolean {
  const query = `(max-width: ${width - 0.02}px)`
  const [narrow, setNarrow] = useState(() => typeof window.matchMedia === 'function' && window.matchMedia(query).matches)
  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return
    const list = window.matchMedia(query)
    const update = (): void => { setNarrow(list.matches) }
    update()
    list.addEventListener('change', update)
    return () => { list.removeEventListener('change', update) }
  }, [query])
  return narrow
}

/**
 * @param props.actions Actions in display order; `false` and `undefined` entries are skipped.
 * @returns The actions as toolbar buttons, or one "更多" button with a menu on a narrow page.
 */
export function ToolActions({ actions }: { actions: readonly (ToolAction | false | undefined)[] }): JSX.Element | null {
  const t = useViewerT()
  const narrow = useNarrow()
  const [open, setOpen] = useState(false)
  const shown = actions.filter((action): action is ToolAction => action !== false && action !== undefined)
  useEffect(() => { if (!narrow) setOpen(false) }, [narrow])
  if (shown.length === 0) return null
  if (!narrow) {
    return <>{shown.map(action => (
      <button key={action.id} type="button" className={css.toolButton} title={action.title} disabled={action.disabled} onClick={action.onSelect}>{action.label}</button>
    ))}</>
  }
  return <Menu open={open} onClose={() => { setOpen(false) }} portal compact align="end"
    anchor={<button type="button" className={css.toolButton} aria-label={t('toolMore')} title={t('toolMore')} aria-haspopup="menu" aria-expanded={open}
      data-tool-more onClick={() => { setOpen(value => !value) }}><IconEllipsisOutlineRegular size={14} /></button>}>
    {shown.map(action => (
      <MenuItemButton key={action.id} disabled={action.disabled} onSelect={() => { setOpen(false); action.onSelect() }}>{action.label}</MenuItemButton>
    ))}
  </Menu>
}
