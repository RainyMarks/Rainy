/** Labelled icon button for window chrome and pane headers. */
import type { ReactNode } from 'react'
import { Button } from './Button.tsx'
import { Tooltip } from './Tooltip.tsx'
import css from './IconAction.module.css'

/**
 * Render an icon-only action whose label is both its accessible name and its tooltip.
 * @param props.label Accessible name and tooltip text.
 * @param props.children Glyph.
 * @param props.onClick Action.
 * @param props.disabled Whether the action is unavailable.
 * @param props.pressed Toggle state, rendered as `aria-pressed`.
 * @param props.expanded Menu state; when set the button also announces a menu popup.
 * @returns The button wrapped in its tooltip.
 */
export function IconAction({ label, children, onClick, disabled, pressed, expanded }: {
  label: string
  children: ReactNode
  onClick: () => void
  disabled?: boolean | undefined
  pressed?: boolean | undefined
  expanded?: boolean | undefined
}) {
  return <Tooltip label={label} side="bottom" portal>
    <Button size="sm" className={css.iconButton} aria-label={label} disabled={disabled} aria-pressed={pressed}
      aria-expanded={expanded} aria-haspopup={expanded === undefined ? undefined : 'menu'}
      onClick={onClick}>{children}</Button>
  </Tooltip>
}
