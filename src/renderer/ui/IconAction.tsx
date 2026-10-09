/** Labelled icon button shared by the workbench chrome. */
import type { ReactNode } from 'react'
import { Button, Tooltip } from '@deepseek-ai/dsh-client-ui-primitives'
import css from './IdeShell.module.css'

/**
 * Render an icon-only action whose label is both its accessible name and its tooltip.
 * @param props - label, glyph, action and optional toggle or menu state.
 * @returns the button wrapped in its tooltip.
 */
export function IconAction({ label, children, onClick, disabled, pressed, expanded }: {
  label: string
  children: ReactNode
  onClick: () => void
  disabled?: boolean
  pressed?: boolean
  expanded?: boolean
}) {
  return <Tooltip label={label} side="bottom" portal>
    <Button size="sm" className={css.iconButton} aria-label={label} disabled={disabled} aria-pressed={pressed}
      aria-expanded={expanded} aria-haspopup={expanded === undefined ? undefined : 'menu'}
      onClick={onClick}>{children}</Button>
  </Tooltip>
}
