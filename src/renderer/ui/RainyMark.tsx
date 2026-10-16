/** The Rainy mark drawn inline, so it takes the accent color and its three bars can fall like rain. */
import { useId, useState, type AnimationEvent, type CSSProperties } from 'react'
import clsx from 'clsx'
import css from './RainyMark.module.css'

/** Bars of `resources/icon.svg`: centre, length; each is 9 wide and leans 19.5° like a rain streak. */
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
 * Render the mark. Pointing at it lets one shower fall through it; the shower always finishes, so leaving early does
 * not snap the bars back. Every movement stops under `prefers-reduced-motion`.
 * @param props Size and motion.
 * @returns A decorative svg.
 */
export function RainyMark({ size, raining = false, entrance = false, className }: RainyMarkProps): JSX.Element {
  const id = useId().replaceAll(':', '')
  const [shower, setShower] = useState(false)
  // Kept only until the first animation ends: putting the entrance back after a shower would play it again.
  const [entering, setEntering] = useState(entrance)
  const finish = (event: AnimationEvent<SVGSVGElement>): void => {
    if (!(event.target instanceof Element) || event.target.getAttribute('data-bar') !== String(BARS.length - 1)) return
    setShower(false)
    setEntering(false)
  }
  return (
    <svg width={size} height={size} viewBox="0 0 100 100" className={clsx(css.mark, className)} aria-hidden="true" data-rainy-mark
      data-raining={raining || undefined} data-shower={(shower && !raining) || undefined} data-entrance={entering || undefined}
      onPointerEnter={() => { if (!raining) setShower(true) }} onAnimationEnd={finish}>
      <defs>
        <linearGradient id={`rainy-mark-fill-${id}`} x1="0" y1="0" x2="0.4" y2="1">
          <stop offset="0" className={css.fillStart} />
          <stop offset="1" className={css.fillEnd} />
        </linearGradient>
        <clipPath id={`rainy-mark-clip-${id}`}><rect width="100" height="100" rx="22" /></clipPath>
      </defs>
      <rect width="100" height="100" rx="22" fill={`url(#rainy-mark-fill-${id})`} />
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
