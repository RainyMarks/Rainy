/** Keeps a transcript at its newest output until the user scrolls away, without fighting a scroll in progress. */
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from 'react'

/** Distance from the bottom, in pixels, that still counts as reading the newest output. */
export const PIN_TOLERANCE = 40
/**
 * Output does not move the view for this long after the user scrolls by hand.
 * A smooth wheel scroll moves the view over several frames; jumping to the bottom in between would undo it.
 */
export const USER_SCROLL_COOLDOWN_MS = 150
/** The jump-to-latest button shows once the view is this far above the bottom. */
export const JUMP_DISTANCE = 120

const UP_KEYS = new Set(['PageUp', 'ArrowUp', 'Home'])

/** What {@link useScrollPin} gives the transcript. */
export interface ScrollPin {
  /** Attach to the scrolling element. */
  ref: RefObject<HTMLDivElement>
  /** The view is far enough above the bottom to offer a jump back. */
  away: boolean
  /** Follow the newest output again and scroll to it. */
  follow(smooth?: boolean): void
}

/**
 * Follow the bottom of a scrolling element across renders.
 * Scrolling up by wheel, keyboard, touch or scrollbar stops following at once; coming back within {@link PIN_TOLERANCE} of the bottom resumes it.
 * @param resetKey Following restarts whenever this changes (the chat shown).
 * @returns The pin.
 */
export function useScrollPin(resetKey: unknown): ScrollPin {
  const ref = useRef<HTMLDivElement>(null)
  const following = useRef(true)
  /** Last hand scroll in any direction, and last one upward (`performance.now()`). */
  const userAt = useRef(Number.NEGATIVE_INFINITY)
  const upAt = useRef(Number.NEGATIVE_INFINITY)
  const dragging = useRef(false)
  const lastTop = useRef(0)
  const [away, setAway] = useState(false)

  useLayoutEffect(() => {
    following.current = true
    userAt.current = Number.NEGATIVE_INFINITY
    upAt.current = Number.NEGATIVE_INFINITY
    setAway(false)
  }, [resetKey])

  // Runs after every render: new output grows the content without firing a scroll event.
  useLayoutEffect(() => {
    const element = ref.current
    if (element === null) return
    if (following.current && !dragging.current && performance.now() - userAt.current >= USER_SCROLL_COOLDOWN_MS) {
      element.scrollTop = element.scrollHeight
      lastTop.current = element.scrollTop
      return
    }
    setAway(element.scrollHeight - element.scrollTop - element.clientHeight > JUMP_DISTANCE)
  })

  useEffect(() => {
    const element = ref.current
    if (element === null) return undefined
    const scrolledUp = (): void => {
      const now = performance.now()
      userAt.current = now
      upAt.current = now
      following.current = false
    }
    const onWheel = (event: WheelEvent): void => {
      if (event.deltaY < 0) scrolledUp()
      else userAt.current = performance.now()
    }
    const onTouch = (): void => { userAt.current = performance.now() }
    // The scrollbar belongs to the scrolling element itself; presses on content have a child as target.
    const onPointerDown = (event: PointerEvent): void => { if (event.target === element) dragging.current = true }
    const onPointerUp = (): void => { dragging.current = false }
    const onKey = (event: KeyboardEvent): void => {
      if (!UP_KEYS.has(event.key) && !(event.key === ' ' && event.shiftKey)) return
      const target = event.target
      if (target instanceof Element && target.closest('input, textarea, select, [contenteditable="true"], [role="listbox"], [role="menu"]') !== null) return
      // Keyboard scrolling reaches the transcript only from the page body or from inside it.
      if (target instanceof Node && target !== element.ownerDocument.body && !element.contains(target)) return
      scrolledUp()
    }
    const onScroll = (): void => {
      const top = element.scrollTop
      const movedUp = top < lastTop.current - 1
      lastTop.current = top
      const distance = element.scrollHeight - top - element.clientHeight
      // Growing content never fires a scroll event, so an upward move is the user (or a shrinking page clamping the view at the bottom).
      if (movedUp && distance > 1) {
        upAt.current = performance.now()
        following.current = false
      } else if (distance <= PIN_TOLERANCE && performance.now() - upAt.current >= USER_SCROLL_COOLDOWN_MS) {
        following.current = true
      }
      setAway(distance > JUMP_DISTANCE)
    }
    const doc = element.ownerDocument
    element.addEventListener('wheel', onWheel, { passive: true })
    element.addEventListener('touchmove', onTouch, { passive: true })
    element.addEventListener('pointerdown', onPointerDown)
    element.addEventListener('scroll', onScroll, { passive: true })
    doc.addEventListener('pointerup', onPointerUp, true)
    doc.addEventListener('pointercancel', onPointerUp, true)
    doc.addEventListener('keydown', onKey, true)
    return () => {
      element.removeEventListener('wheel', onWheel)
      element.removeEventListener('touchmove', onTouch)
      element.removeEventListener('pointerdown', onPointerDown)
      element.removeEventListener('scroll', onScroll)
      doc.removeEventListener('pointerup', onPointerUp, true)
      doc.removeEventListener('pointercancel', onPointerUp, true)
      doc.removeEventListener('keydown', onKey, true)
    }
  }, [])

  const follow = useCallback((smooth = false) => {
    const element = ref.current
    following.current = true
    userAt.current = Number.NEGATIVE_INFINITY
    upAt.current = Number.NEGATIVE_INFINITY
    setAway(false)
    if (element !== null) element.scrollTo({ top: element.scrollHeight, behavior: smooth ? 'smooth' : 'auto' })
  }, [])

  return { ref, away, follow }
}
