/** Anchor-preserving tooltips; an optional body portal escapes clipping containers and stacking contexts that cap the bubble's z-index. */

import { cloneElement, useCallback, useEffect, useRef, useState } from 'react'
import type { FocusEventHandler, MouseEventHandler, MutableRefObject, ReactElement, Ref } from 'react'
import { createPortal } from 'react-dom'
import css from './Tooltip.module.css'
// Tooltips take the wide answer — any key returns to the keyboard. Focus rings read the
// narrower `data-input-modality` attribute the same module publishes.
import { pointerModality } from './input-modality.ts'

/** Bubble placement relative to the anchor. */
export type TooltipSide = 'right' | 'bottom' | 'top'

/** Props Tooltip injects into its anchor child; the child's own handlers are chained ahead of the tooltip's. */
interface AnchorProps {
  ref?: Ref<HTMLElement> | undefined
  onMouseEnter?: MouseEventHandler | undefined
  onMouseLeave?: MouseEventHandler | undefined
  onClick?: MouseEventHandler | undefined
  onFocus?: FocusEventHandler | undefined
  onBlur?: FocusEventHandler | undefined
}

type TooltipLabel = string | (() => string)

/**
 * Attach a hover/focus tooltip to an anchor element.
 * @param props.label - bubble text, or a resolver evaluated only while visible; an empty string shows only shortcut keys.
 * @param props.shortcutKeys - effective key labels rendered as platform-formatted keycaps after optional text.
 * @param props.side - placement relative to the anchor (default 'right').
 * @param props.align - horizontal anchor-edge alignment for 'bottom'/'top' bubbles: 'end' pins
 * the bubble's right edge to the anchor's (for anchors beside other hover surfaces the centered
 * bubble would overlap); default 'center'. Ignored for side 'right'.
 * @param props.portal - render the bubble under document.body, so an ancestor's clipping or its
 * stacking context (which confines the bubble's z-index to that context) cannot hide it.
 * @param props.delayMs - hover delay in milliseconds; keyboard focus remains immediate.
 * @param props.gap - anchor-to-bubble distance in pixels for 'bottom'/'top' bubbles (default 8);
 * ignored for side 'right'.
 * @param props.disabled - suppress the bubble while true; the anchor renders identically so
 * toggling never remounts it (which would cut its CSS transitions).
 * @param props.maxWidth - bubble width cap in pixels, for labels long enough that the default
 * half-viewport cap would render a slab wider than the surface the anchor sits on.
 * @param props.children - a single anchor element; its own ref (callback or object) is forwarded alongside the tooltip's.
 * @returns the cloned anchor plus a fixed-position bubble, optionally portaled to the body.
 * The bubble stays hidden until ResizeObserver supplies its size for viewport fitting; clicking the
 * anchor dismisses the bubble until the next trigger, and focus arriving after a pointer
 * interaction (a closing menu refocusing its trigger) never raises it.
 */
export function Tooltip({ label, side = 'right', align = 'center', delayMs = 0, gap = 8, disabled = false, portal = false, maxWidth, children }: { label: TooltipLabel; side?: TooltipSide; align?: 'center' | 'end'; delayMs?: number; gap?: number; disabled?: boolean; portal?: boolean; maxWidth?: number; children: ReactElement<AnchorProps> }) {
  const anchor = useRef<HTMLElement | null>(null)
  // React 18 keeps the element's ref outside props; forward it so wrapping an
  // anchor in Tooltip never silently severs the owner's ref.
  const childRef = (children as ReactElement<AnchorProps> & { ref?: Ref<HTMLElement> }).ref
  const mergedRef = useCallback((el: HTMLElement | null) => {
    anchor.current = el
    if (typeof childRef === 'function') childRef(el)
    else if (childRef != null) (childRef as MutableRefObject<HTMLElement | null>).current = el
  }, [childRef])
  // The anchor's edges rather than final coordinates: a vertical flip has to
  // re-derive the bubble's own top from the opposite edge.
  const [pos, setPos] = useState<{ x: number; top: number; bottom: number } | null>(null)
  const bubble = useRef<HTMLSpanElement | null>(null)
  const resolvedLabel = pos === null
    ? null
    : typeof label === 'function' ? label() : label
  const y = pos === null
    ? 0
    : side === 'right'
      ? pos.top + (pos.bottom - pos.top) / 2
      : side === 'top' ? pos.top - gap : pos.bottom + gap
  const showTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  // Hover and focus are independent triggers: the bubble hides only after
  // BOTH clear (hovering away from a focused anchor must not drop it).
  const triggers = useRef({ hover: false, focus: false })

  const visible = pos !== null && !disabled
  // ResizeObserver supplies the laid-out border box; fitting never reads
  // geometry after a position write or needs a React commit to flip sides.
  useEffect(() => {
    const el = bubble.current
    if (pos === null || !visible || el === null) return
    const edgeMargin = 12
    let size: ResizeObserverSize | undefined
    let placement = side
    const fit = () => {
      if (size === undefined) return
      const { inlineSize: width, blockSize: height } = size
      const offset = side === 'right' ? 0 : align === 'end' ? width : width / 2
      const left = Math.max(edgeMargin, Math.min(pos.x - offset, window.innerWidth - edgeMargin - width))
      const fitsBelow = pos.bottom + gap + height <= window.innerHeight - edgeMargin
      const fitsAbove = pos.top - gap - height >= edgeMargin
      if (placement === 'bottom' && !fitsBelow && fitsAbove) placement = 'top'
      else if (placement === 'top' && !fitsAbove && fitsBelow) placement = 'bottom'
      el.style.left = `${left + offset}px`
      el.style.top = `${placement === 'right' ? (pos.top + pos.bottom) / 2
        : placement === 'top' ? pos.top - gap : pos.bottom + gap}px`
      el.dataset.side = placement
      el.style.visibility = 'visible'
    }
    const observer = new ResizeObserver((entries) => {
      size = entries[0]?.borderBoxSize[0]
      fit()
    })
    observer.observe(el, { box: 'border-box' })
    window.addEventListener('resize', fit)
    return () => {
      observer.disconnect()
      window.removeEventListener('resize', fit)
    }
  }, [align, gap, pos, side, visible])

  // Disabling mid-hover (e.g. clicking a rail control expands the sidebar)
  // must drop an already-visible bubble: no mouseleave fires.
  const cancelShow = useCallback(() => {
    if (showTimer.current === null) return
    clearTimeout(showTimer.current)
    showTimer.current = null
  }, [])
  useEffect(() => {
    if (disabled) {
      cancelShow()
      triggers.current = { hover: false, focus: false }
      setPos(null)
    }
    return cancelShow
  }, [cancelShow, disabled])

  const show = () => {
    if (disabled) return
    const el = anchor.current
    /* v8 ignore next -- the ref is attached by event time: events fire on the cloned anchor. */
    if (el === null) return
    const r = el.getBoundingClientRect()
    setPos({
      x: side === 'right' ? r.right + 10 : align === 'end' ? r.right : r.left + r.width / 2,
      top: r.top,
      bottom: r.bottom,
    })
  }
  const showAfterHoverDelay = () => {
    cancelShow()
    if (delayMs <= 0) {
      show()
      return
    }
    showTimer.current = setTimeout(() => {
      showTimer.current = null
      show()
    }, delayMs)
  }
  const withdraw = () => { setPos(null) }
  const hide = () => {
    cancelShow()
    if (!triggers.current.hover && !triggers.current.focus) withdraw()
  }

  const content = visible && (
    <span
      ref={bubble}
      className={css.bubble}
      data-side={side}
      data-portal={portal || undefined}
      data-align={align}
      style={{ left: pos.x, top: y, visibility: 'hidden', ...maxWidth === undefined ? {} : { maxWidth } }}
      role="tooltip"
    >
      {resolvedLabel && <span className={css.label}>{resolvedLabel}</span>}
    </span>
  )

  return (
    <>
      {cloneElement(children, {
        ref: mergedRef,
        onMouseEnter: (e) => { children.props.onMouseEnter?.(e); triggers.current.hover = true; showAfterHoverDelay() },
        onMouseLeave: (e) => { children.props.onMouseLeave?.(e); triggers.current.hover = false; cancelShow(); withdraw() },
        onClick: (e) => { children.props.onClick?.(e); triggers.current.focus = false; cancelShow(); withdraw() },
        onFocus: (e) => { children.props.onFocus?.(e); if (pointerModality()) return; triggers.current.focus = true; cancelShow(); show() },
        onBlur: (e) => { children.props.onBlur?.(e); triggers.current.focus = false; hide() },
      })}
      {portal ? (content !== false && createPortal(content, document.body)) : content}
    </>
  )
}
