import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import type { CSSProperties, ReactNode } from 'react'
import { createPortal } from 'react-dom'
import clsx from 'clsx'
import { focusWithoutRing } from './focus.ts'
import { observeComposition } from './keyboard-composition.ts'
import css from './Menu.module.css'

/** Props for one component-rendered menu row. */
export interface MenuItemButtonProps {
  /** Visible row label. */
  children: ReactNode
  /** Leading icon. */
  icon?: ReactNode
  /** Whether the row cannot be activated. */
  disabled?: boolean
  /** Destructive row: error-colored text/icon and danger hover fill. */
  danger?: boolean
  /** Start a new group with a hairline above this row. */
  separatorBefore?: boolean
  /** Row activation callback. */
  onSelect: () => void
}

/** One `role="menuitem"` row inside a {@link Menu}. */
export function MenuItemButton({
  children, icon, disabled = false, danger = false, separatorBefore = false, onSelect,
}: MenuItemButtonProps): JSX.Element {
  return (
    <div className={css.itemWrap}>
      {separatorBefore && <div className={css.separator} role="separator" />}
      <button
        type="button"
        role="menuitem"
        className={clsx(css.item, danger && css.danger)}
        disabled={disabled}
        onClick={onSelect}
      >
        {icon !== undefined && <span className={css.itemIcon}>{icon}</span>}
        <span className={css.itemLabel}>{children}</span>
      </button>
    </div>
  )
}

const MEASURE_STYLE: CSSProperties = { visibility: 'hidden', left: 0, top: 0 }
const VIEWPORT_MARGIN = 12

/**
 * Lightweight single-level anchored dropdown menu for viewer toolbars.
 * Positions portaled menus on open, scroll, and resize (no continuous rAF polling).
 */
export function Menu({ open, anchor, children, onClose, align = 'start', side = 'bottom', portal = false, compact = false, autoFocus = false, className, listClassName }: {
  open: boolean
  autoFocus?: boolean
  anchor: ReactNode
  children?: ReactNode
  onClose: () => void
  align?: 'start' | 'end'
  side?: 'bottom' | 'top'
  portal?: boolean
  compact?: boolean
  className?: string | undefined
  listClassName?: string | undefined
}): JSX.Element {
  const rootRef = useRef<HTMLSpanElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  const triggerRef = useRef<HTMLElement | null>(null)
  const openRef = useRef(open)
  openRef.current = open
  const [fixedPos, setFixedPos] = useState<CSSProperties | null>(null)

  const refocusAnchor = (navigation = false): void => {
    const trigger = triggerRef.current
    const target = trigger !== null && document.contains(trigger) && !(trigger as HTMLButtonElement).disabled
      ? trigger : rootRef.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')
    if (target == null) return
    if (navigation) target.focus()
    else focusWithoutRing(target)
  }

  useLayoutEffect(() => {
    if (!open || !portal) { setFixedPos(null); return }
    const place = (): void => {
      const rect = rootRef.current?.getBoundingClientRect()
      if (rect === undefined) return
      const listEl = listRef.current
      const lw = listEl?.offsetWidth ?? 0
      const lh = listEl?.offsetHeight ?? 0
      let x = align === 'start' ? rect.left : rect.right - lw
      let y = side === 'bottom' ? rect.bottom + 4 : rect.top - lh - 4
      if (lw > 0) x = Math.min(Math.max(x, VIEWPORT_MARGIN), window.innerWidth - lw - VIEWPORT_MARGIN)
      if (lh > 0) y = Math.min(Math.max(y, VIEWPORT_MARGIN), window.innerHeight - lh - VIEWPORT_MARGIN)
      setFixedPos(current => current?.left === x && current.top === y ? current : { left: x, top: y })
    }
    place()
    window.addEventListener('scroll', place, true)
    window.addEventListener('resize', place)
    return () => {
      window.removeEventListener('scroll', place, true)
      window.removeEventListener('resize', place)
    }
  }, [open, portal, align, side])

  useEffect(() => {
    if (!open) { triggerRef.current = null; return }
    const active = document.activeElement
    triggerRef.current = active instanceof HTMLElement && rootRef.current?.contains(active) === true ? active : null
    if (autoFocus) {
      const first = listRef.current?.querySelector<HTMLButtonElement>('button:not(:disabled)')
      if (first != null) focusWithoutRing(first)
    }
  }, [open, autoFocus])

  useEffect(() => {
    if (!open) return
    const composition = observeComposition(document)
    const onPointerDown = (event: PointerEvent): void => {
      if (!(event.target instanceof Node)) return
      if (rootRef.current?.contains(event.target) === true || listRef.current?.contains(event.target) === true) return
      onClose()
    }
    const onKeyDown = (event: KeyboardEvent): void => {
      if (composition.guards(event) || event.defaultPrevented || event.ctrlKey || event.altKey || event.metaKey) return
      const focused = document.activeElement
      const insideList = listRef.current?.contains(focused) === true
      const anchored = rootRef.current?.contains(focused) === true || insideList
      if (event.key === 'Escape' && !event.shiftKey) {
        event.preventDefault()
        if (event.repeat) return
        onClose()
        if (anchored || autoFocus) refocusAnchor()
        return
      }
      if (!anchored || listRef.current === null) return
      if (event.key === 'Tab') {
        if (event.shiftKey) { event.preventDefault(); onClose(); refocusAnchor(true); return }
        if (!insideList) {
          const first = listRef.current.querySelector<HTMLButtonElement>('button:not(:disabled)')
          if (first !== null) { event.preventDefault(); first.focus() }
        }
        return
      }
      if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
      const buttons = Array.from(listRef.current.querySelectorAll<HTMLButtonElement>('button:not(:disabled)'))
      if (buttons.length === 0) return
      const index = buttons.indexOf(focused as HTMLButtonElement)
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? buttons.length - 1
        : index < 0 ? (event.key === 'ArrowDown' ? 0 : buttons.length - 1)
          : (index + (event.key === 'ArrowDown' ? 1 : -1) + buttons.length) % buttons.length
      event.preventDefault()
      buttons[next]?.focus()
    }
    document.addEventListener('pointerdown', onPointerDown)
    document.addEventListener('keydown', onKeyDown, true)
    return () => {
      composition.dispose()
      document.removeEventListener('pointerdown', onPointerDown)
      document.removeEventListener('keydown', onKeyDown, true)
    }
  }, [open, onClose, autoFocus])

  const list = open && (
    <div
      ref={listRef}
      className={clsx(css.list, listClassName, compact && css.compactList, portal && css.portal, side === 'top' && !portal && css.sideTop, align === 'end' && !portal && css.alignEnd)}
      style={portal ? fixedPos ?? MEASURE_STYLE : undefined}
      role="menu"
      onClick={(event) => {
        event.stopPropagation()
        if (event.target instanceof Element && event.target.closest('button[role="menuitem"]') !== null) {
          queueMicrotask(() => {
            if (openRef.current) return
            const active = document.activeElement
            if (active === null || active === document.body || listRef.current?.contains(active) === true) refocusAnchor()
          })
        }
      }}
    >
      <div className={css.viewport} role="presentation">{children}</div>
    </div>
  )

  return (
    <span ref={rootRef} className={clsx(css.root, className)}>
      {anchor}
      {portal ? (list !== false && createPortal(list, document.body)) : list}
    </span>
  )
}
