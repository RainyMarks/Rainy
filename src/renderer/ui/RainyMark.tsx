/** The Rainy mark drawn inline, so it takes the accent color and its three bars can fall like rain. */
import { useEffect, useId, useRef, useState, type AnimationEvent, type CSSProperties } from 'react'
import clsx from 'clsx'
import css from './RainyMark.module.css'

/** Bars of `resources/icon.svg`, whose tile and sheen this repeats on the accent ramp: centre, length; each is 9 wide and
 * leans 19.5° like a rain streak. */
const BARS = [
  { x: 29, y: 37, length: 38.74 },
  { x: 48.72, y: 50.25, length: 57.7 },
  { x: 70, y: 59, length: 45.06 },
] as const

/** How the mark moves. */
export interface RainyMarkProps {
  /** Edge in px. */
  size: number
  /** Rain falls through the mark until this turns false, for example while the agent works. */
  raining?: boolean | undefined
  /** The bars drop into place once when the mark first appears. */
  entrance?: boolean | undefined
  className?: string | undefined
}

/**
 * Render the mark. Pointing at it lets one shower fall through it. A shower, and the rain after `raining` turns false,
 * always finish the fall in progress, so the bars never snap back. Every movement stops under `prefers-reduced-motion`.
 * @param props Size and motion.
 * @returns A decorative svg.
 */
export function RainyMark({ size, raining = false, entrance = false, className }: RainyMarkProps): JSX.Element {
  const id = useId().replaceAll(':', '')
  const ref = useRef<SVGSVGElement>(null)
  const [shower, setShower] = useState(false)
  // Kept only until the first animation ends: putting the entrance back after a shower would play it again.
  const [entering, setEntering] = useState(entrance)
  // The rain keeps its attribute after `raining` turns false until each bar ends its current fall.
  const [wasRaining, setWasRaining] = useState(raining)
  const [settling, setSettling] = useState(false)
  // A new element starts fresh CSS animations when rain resumes while the old ones are being wound down.
  const [generation, setGeneration] = useState(0)
  if (raining !== wasRaining) {
    setWasRaining(raining)
    setSettling(!raining)
    if (raining && settling) setGeneration(generation + 1)
  }
  useEffect(() => {
    const svg = ref.current
    if (!settling || svg === null) return undefined
    let current = true
    const ending: Promise<unknown>[] = []
    for (const animation of typeof svg.getAnimations === 'function' ? svg.getAnimations({ subtree: true }) : []) {
      const iteration = animation.effect?.getComputedTiming().currentIteration
      if (typeof iteration === 'number') {
        animation.effect?.updateTiming({ iterations: iteration + 1 })
        ending.push(animation.finished.catch(() => undefined))
      } else {
        // A bar still waiting out its delay is at rest already. A cancelled animation's `finished` is a new promise
        // that only settles if it plays again, so it is not waited for.
        animation.cancel()
      }
    }
    void Promise.all(ending).then(() => { if (current) setSettling(false) })
    return () => { current = false }
  }, [settling])
  const finish = (event: AnimationEvent<SVGSVGElement>): void => {
    if (!(event.target instanceof Element) || event.target.getAttribute('data-bar') !== String(BARS.length - 1)) return
    setShower(false)
    setEntering(false)
  }
  const rain = raining || settling
  return (
    <svg key={generation} ref={ref} width={size} height={size} viewBox="0 0 100 100" className={clsx(css.mark, className)} aria-hidden="true"
      data-rainy-mark data-raining={rain || undefined} data-shower={(shower && !rain) || undefined} data-entrance={entering || undefined}
      onPointerEnter={() => { if (!rain) setShower(true) }} onAnimationEnd={finish}>
      <defs>
        <linearGradient id={`rainy-mark-fill-${id}`} x1="0" y1="0" x2="0.4" y2="1">
          <stop offset="0" className={css.fillStart} />
          <stop offset="1" className={css.fillEnd} />
        </linearGradient>
        <linearGradient id={`rainy-mark-sheen-${id}`} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#fff" stopOpacity="0.16" />
          <stop offset="0.5" stopColor="#fff" stopOpacity="0" />
        </linearGradient>
        <clipPath id={`rainy-mark-clip-${id}`}><rect width="100" height="100" rx="22" /></clipPath>
      </defs>
      <rect width="100" height="100" rx="22" fill={`url(#rainy-mark-fill-${id})`} />
      <rect width="100" height="100" rx="22" fill={`url(#rainy-mark-sheen-${id})`} />
      <g clipPath={`url(#rainy-mark-clip-${id})`}>
        {BARS.map((bar, index) => (
          <g key={index} transform={`translate(${bar.x} ${bar.y}) rotate(19.5)`}>
            <rect className={css.bar} data-bar={index} style={{ '--rainy-bar': index } as CSSProperties}
              x={-4.5} y={-bar.length / 2} width={9} height={bar.length} rx={4.5} />
          </g>
        ))}
      </g>
    </svg>
  )
}
