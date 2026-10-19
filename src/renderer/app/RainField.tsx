/** Rain behind an empty view; a click on the view's background makes a ripple. Off with Settings → Appearance. */
import { useEffect, useRef } from 'react'
import clsx from 'clsx'
import { usePrefs } from '../prefs.ts'
import { RainPainter, RainSim } from './rain.ts'
import css from './RainField.module.css'

/** Longest step a frame advances, so a view resumed after a pause does not jump. */
const MAX_STEP = 0.05
/** Shortest time between frames: about 60 per second on any display (120–240 Hz panels skip frames), and 24 while the
 * window is in the background, where the rain is only glanced at. A little under 1/60 s keeps a 60 Hz display at 60. */
const FRAME_MS = 1000 / 60 - 2
const BACKGROUND_FRAME_MS = 1000 / 24
/** The canvas resolution stops at 1.5 device pixels per CSS pixel: thin translucent streaks look the same, and a
 * 225% display redraws about half the pixels it would at full resolution. */
const MAX_RATIO = 1.5
/** Clicks on these draw no ripple: they belong to the control under the pointer. */
const INTERACTIVE = 'a, button, input, textarea, select, label, [role="button"], [role="menu"], [contenteditable]'
/** Attributes `prefs.ts` sets on <body> for the color mode, scheme and accent. */
const THEME_ATTRIBUTES = ['data-rainy-dark', 'data-color-scheme', 'data-accent']

/**
 * A canvas filling its positioned parent, behind the parent's other children.
 * @param props.className Extra class, for example to change the mask around the view's content.
 * @returns The canvas, or nothing while rain effects are off.
 */
export function RainField({ className }: { className?: string | undefined }): JSX.Element | null {
  const { rainEffects } = usePrefs()
  const ref = useRef<HTMLCanvasElement>(null)
  useEffect(() => {
    const canvas = ref.current
    return canvas === null ? undefined : startRain(canvas)
  }, [rainEffects])
  return rainEffects ? <canvas ref={ref} className={clsx(css.field, className)} aria-hidden="true" data-rain-field /> : null
}

/**
 * Run the rain on a canvas until the returned function is called. Animation pauses while the window is hidden or the
 * canvas has no size and slows down while the window is in the background; with `prefers-reduced-motion` one still
 * frame is drawn and clicks make no ripple.
 * @param canvas Target; its CSS `color` is the rain color.
 * @returns Stop.
 */
export function startRain(canvas: HTMLCanvasElement): () => void {
  const context = canvas.getContext('2d')
  if (context === null) return () => undefined
  const sim = new RainSim()
  const painter = new RainPainter()
  const still = window.matchMedia('(prefers-reduced-motion: reduce)')
  let ratio = 1
  let frame = 0
  let last = 0
  let painted = { color: '', ratio: 0 }

  const draw = (): void => {
    context.setTransform(ratio, 0, 0, ratio, 0, 0)
    context.clearRect(0, 0, sim.width, sim.height)
    painter.paint(context, sim)
  }
  const moving = (): boolean => !document.hidden && !still.matches && sim.width > 0 && sim.height > 0
  const tick = (now: number): void => {
    frame = 0
    if (last !== 0 && now - last < (document.hasFocus() ? FRAME_MS : BACKGROUND_FRAME_MS)) { schedule(); return }
    sim.step(last === 0 ? 0 : Math.min(MAX_STEP, (now - last) / 1000))
    last = now
    draw()
    schedule()
  }
  function schedule(): void {
    if (!moving()) { last = 0; return }
    if (frame === 0) frame = requestAnimationFrame(tick)
  }
  const recolor = (): void => {
    const color = getComputedStyle(canvas).color
    // The sprites are drawn again only when their color or resolution changed, not for every resize step.
    if (painted.color !== color || painted.ratio !== ratio) {
      painter.setColor(color, ratio)
      painted = { color, ratio }
    }
    if (!moving()) draw()
  }
  const resize = (): void => {
    ratio = Math.min(MAX_RATIO, window.devicePixelRatio || 1)
    const width = canvas.clientWidth
    const height = canvas.clientHeight
    const pixels = { width: Math.round(width * ratio), height: Math.round(height * ratio) }
    // Setting a canvas size clears it and reallocates its backing store even when the size is the same.
    if (canvas.width !== pixels.width || canvas.height !== pixels.height) {
      canvas.width = pixels.width
      canvas.height = pixels.height
    }
    sim.resize(width, height)
    recolor()
    schedule()
  }
  const splash = (event: PointerEvent): void => {
    if (event.button !== 0 || still.matches || (event.target instanceof Element && event.target.closest(INTERACTIVE) !== null)) return
    const box = canvas.getBoundingClientRect()
    sim.splash(event.clientX - box.left, event.clientY - box.top)
    schedule()
  }
  const changeMotion = (): void => { resize() }

  const sized = typeof ResizeObserver === 'undefined' ? undefined : new ResizeObserver(resize)
  sized?.observe(canvas)
  const themed = new MutationObserver(recolor)
  themed.observe(document.body, { attributes: true, attributeFilter: THEME_ATTRIBUTES })
  const parent = canvas.parentElement
  parent?.addEventListener('pointerdown', splash)
  document.addEventListener('visibilitychange', schedule)
  still.addEventListener('change', changeMotion)
  resize()
  return () => {
    if (frame !== 0) cancelAnimationFrame(frame)
    sized?.disconnect()
    themed.disconnect()
    parent?.removeEventListener('pointerdown', splash)
    document.removeEventListener('visibilitychange', schedule)
    still.removeEventListener('change', changeMotion)
  }
}
