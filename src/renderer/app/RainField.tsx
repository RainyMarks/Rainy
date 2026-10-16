/** Rain behind an empty view; a click on the view's background makes a ripple. Off with Settings → Appearance. */
import { useEffect, useRef } from 'react'
import clsx from 'clsx'
import { usePrefs } from '../prefs.ts'
import { RainPainter, RainSim } from './rain.ts'
import css from './RainField.module.css'

/** Longest step a frame advances, so a view resumed after a pause does not jump. */
const MAX_STEP = 0.05
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
 * canvas has no size; with `prefers-reduced-motion` one still frame is drawn and clicks make no ripple.
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

  const draw = (): void => {
    context.setTransform(ratio, 0, 0, ratio, 0, 0)
    context.clearRect(0, 0, sim.width, sim.height)
    painter.paint(context, sim)
  }
  const moving = (): boolean => !document.hidden && !still.matches && sim.width > 0 && sim.height > 0
  const tick = (now: number): void => {
    frame = 0
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
    painter.setColor(getComputedStyle(canvas).color, ratio)
    if (!moving()) draw()
  }
  const resize = (): void => {
    ratio = Math.min(2, window.devicePixelRatio || 1)
    const width = canvas.clientWidth
    const height = canvas.clientHeight
    canvas.width = Math.round(width * ratio)
    canvas.height = Math.round(height * ratio)
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
