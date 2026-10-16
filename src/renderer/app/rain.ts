/**
 * The rain behind empty views: a simulation of slanted streaks, the rings they leave where they land and the ripple a
 * click makes, and a painter that draws it on a 2D canvas. Sizes are CSS pixels and times seconds.
 */

/** Streaks lean like the bars of the Rainy mark: 19.5° from vertical, falling down and to the left. */
export const RAIN_SLANT = (19.5 * Math.PI) / 180
/** Rings are ellipses this much flatter than circles, as if seen on water at an angle. */
export const RING_FLATTEN = 0.42
/** Depth layers, far (0) to near; nearer drops are longer, faster, thicker and brighter. */
export const DEPTHS = 5
/** One drop per this many square pixels, up to {@link MAX_DROPS}. */
export const AREA_PER_DROP = 5200
export const MAX_DROPS = 220
/** Click ripples kept at once; the oldest goes first. */
export const MAX_SPLASHES = 5

const FALL_X = -Math.sin(RAIN_SLANT)
const FALL_Y = Math.cos(RAIN_SLANT)
const GRAVITY = 900
const TAU = Math.PI * 2

/** Streak length of a depth. */
export function dropLength(depth: number): number { return 10 + depth * 7 }
/** Streak opacity of a depth, before the canvas opacity. */
export function dropAlpha(depth: number): number { return 0.14 + depth * 0.08 }
function dropWidth(depth: number): number { return 0.7 + depth * 0.18 }

/** A falling streak; `x`, `y` is its head (lower end). */
export interface Drop {
  x: number
  y: number
  depth: number
  speed: number
  /** The head stops at this height: a landing drop leaves a ring there, the others leave the view. */
  end: number
  lands: boolean
}

/** Expanding rings: one where a drop landed, several where the user clicked. */
export interface Ring {
  x: number
  y: number
  age: number
  life: number
  radius: number
  alpha: number
  waves: number
  click: boolean
}

/** A droplet thrown up by a click. */
export interface Spark {
  x: number
  y: number
  vx: number
  vy: number
  age: number
  life: number
}

/** Ease out: fast at first, settling at 1. */
function settle(t: number): number { return 1 - (1 - t) ** 3 }

/** The rain in one view. `random` is injectable so tests are deterministic. */
export class RainSim {
  width = 0
  height = 0
  readonly drops: Drop[] = []
  readonly rings: Ring[] = []
  readonly sparks: Spark[] = []

  constructor(private readonly random: () => number = Math.random) {}

  /**
   * Fit the view; drops are added or removed to keep the density, and the first ones are spread over the whole height.
   * @param width View width.
   * @param height View height.
   */
  resize(width: number, height: number): void {
    this.width = Math.max(0, width)
    this.height = Math.max(0, height)
    const target = Math.min(MAX_DROPS, Math.round((this.width * this.height) / AREA_PER_DROP))
    this.drops.length = Math.min(this.drops.length, target)
    while (this.drops.length < target) this.drops.push(this.spawn(true))
    for (const drop of this.drops) if (!drop.lands) drop.end = this.height + dropLength(drop.depth)
  }

  /**
   * Advance the rain.
   * @param dt Elapsed time; callers cap it so a resumed view does not jump.
   */
  step(dt: number): void {
    for (let index = 0; index < this.drops.length; index++) {
      const drop = this.drops[index]!
      drop.x += FALL_X * drop.speed * dt
      drop.y += FALL_Y * drop.speed * dt
      if (drop.y < drop.end && drop.x > -dropLength(drop.depth)) continue
      if (drop.lands && drop.y >= drop.end) {
        this.rings.push({ x: drop.x, y: drop.end, age: 0, life: 0.45 + drop.depth * 0.08, radius: 4 + drop.depth * 3,
          alpha: Math.min(1, dropAlpha(drop.depth) * 1.3), waves: 1, click: false })
      }
      this.drops[index] = this.spawn(false)
    }
    age(this.rings, dt)
    for (const spark of this.sparks) {
      spark.vy += GRAVITY * dt
      spark.x += spark.vx * dt
      spark.y += spark.vy * dt
    }
    age(this.sparks, dt)
  }

  /**
   * A drop hits the water where the user clicked: three rings and a few droplets.
   * @param x Horizontal position in the view.
   * @param y Vertical position in the view.
   */
  splash(x: number, y: number): void {
    const clicks = this.rings.filter(ring => ring.click)
    if (clicks.length >= MAX_SPLASHES) this.rings.splice(this.rings.indexOf(clicks[0]!), 1)
    const radius = Math.min(220, Math.max(90, Math.min(this.width, this.height) * 0.3))
    this.rings.push({ x, y, age: 0, life: 1.7, radius, alpha: 0.9, waves: 3, click: true })
    for (let count = 0; count < 7; count++) {
      const angle = -Math.PI / 2 + (this.random() - 0.5) * 2.4
      const speed = 120 + this.random() * 140
      this.sparks.push({ x, y, vx: Math.cos(angle) * speed, vy: Math.sin(angle) * speed, age: 0, life: 0.45 + this.random() * 0.3 })
    }
  }

  /**
   * How click ripples move a drop: the passing wave front pushes it outward and lights it.
   * @param drop A drop.
   * @returns Offset and extra brightness (0 when no wave is near).
   */
  disturbance(drop: Drop): { dx: number; dy: number; boost: number } {
    let dx = 0
    let dy = 0
    let boost = 0
    for (const ring of this.rings) {
      if (!ring.click) continue
      const t = ring.age / ring.life
      const front = ring.radius * settle(t)
      const ox = drop.x - ring.x
      const oy = (drop.y - ring.y) / RING_FLATTEN
      const distance = Math.hypot(ox, oy)
      const near = Math.max(0, 1 - Math.abs(distance - front) / 36) * (1 - t)
      if (near === 0 || distance === 0) continue
      dx += (ox / distance) * near * 9
      dy += (oy / distance) * near * 9 * RING_FLATTEN
      boost += near * 1.6
    }
    return { dx, dy, boost }
  }

  private spawn(scattered: boolean): Drop {
    const depth = Math.min(DEPTHS - 1, Math.floor(this.random() ** 1.6 * DEPTHS))
    const length = dropLength(depth)
    // Drops drift left as they fall, so they start up to a slant's width to the right of the view.
    const x = this.random() * (this.width + this.height * Math.tan(RAIN_SLANT))
    const y = scattered ? -length + this.random() * (this.height + length) : -length - this.random() * 60
    const landing = this.height * (0.3 + this.random() * 0.7)
    // A scattered drop already below its landing point falls out of the view instead of ringing at once.
    const lands = this.random() < 0.45 && landing > y
    return { x, y, depth, speed: 360 + depth * 140 + this.random() * 80, end: lands ? landing : this.height + length, lands }
  }
}

function age<T extends { age: number; life: number }>(items: T[], dt: number): void {
  let kept = 0
  for (const item of items) {
    item.age += dt
    if (item.age < item.life) items[kept++] = item
  }
  items.length = kept
}

/** Draws a {@link RainSim}; streaks are pre-drawn sprites, one per depth, fading from tail to head. */
export class RainPainter {
  private sprites: { image: HTMLCanvasElement; width: number; height: number; pad: number; rise: number }[] = []
  private color = 'transparent'

  /**
   * Redraw the sprites in a new color or pixel ratio.
   * @param color Any CSS color.
   * @param ratio Device pixels per CSS pixel.
   */
  setColor(color: string, ratio: number): void {
    this.color = color
    this.sprites = []
    for (let depth = 0; depth < DEPTHS; depth++) {
      const length = dropLength(depth)
      const width = dropWidth(depth)
      const run = length * Math.sin(RAIN_SLANT)
      const rise = length * Math.cos(RAIN_SLANT)
      const pad = width + 1
      const image = document.createElement('canvas')
      image.width = Math.ceil((run + pad * 2) * ratio)
      image.height = Math.ceil((rise + pad * 2) * ratio)
      const context = image.getContext('2d')
      if (context === null) { this.sprites = []; return }
      context.scale(ratio, ratio)
      context.strokeStyle = color
      context.lineWidth = width
      context.lineCap = 'round'
      context.beginPath()
      context.moveTo(pad + run, pad)
      context.lineTo(pad, pad + rise)
      context.stroke()
      // Fade the tail by masking alpha, so the color string never needs parsing.
      context.globalCompositeOperation = 'destination-in'
      const fade = context.createLinearGradient(pad + run, pad, pad, pad + rise)
      fade.addColorStop(0, 'rgba(0, 0, 0, 0)')
      fade.addColorStop(1, 'rgba(0, 0, 0, 1)')
      context.fillStyle = fade
      context.fillRect(0, 0, run + pad * 2, rise + pad * 2)
      this.sprites.push({ image, width: image.width / ratio, height: image.height / ratio, pad, rise })
    }
  }

  /**
   * Draw one frame on a cleared context already scaled to CSS pixels.
   * @param context Target.
   * @param sim The rain.
   */
  paint(context: CanvasRenderingContext2D, sim: RainSim): void {
    if (this.sprites.length < DEPTHS) return
    const clicked = sim.rings.some(ring => ring.click)
    for (const drop of sim.drops) {
      const sprite = this.sprites[drop.depth]!
      const { dx, dy, boost } = clicked ? sim.disturbance(drop) : { dx: 0, dy: 0, boost: 0 }
      context.globalAlpha = Math.min(1, dropAlpha(drop.depth) * (1 + boost))
      context.drawImage(sprite.image, drop.x + dx - sprite.pad, drop.y + dy - sprite.pad - sprite.rise, sprite.width, sprite.height)
    }
    context.strokeStyle = this.color
    context.fillStyle = this.color
    for (const ring of sim.rings) {
      const t = ring.age / ring.life
      context.lineWidth = ring.click ? 1.2 : 0.9
      for (let wave = 0; wave < ring.waves; wave++) {
        const local = (t - wave * 0.14) / (1 - wave * 0.14)
        if (local <= 0) continue
        const radius = ring.radius * (1 - wave * 0.22) * settle(local)
        context.globalAlpha = ring.alpha * (1 - local) ** 1.6 * (1 - wave * 0.25)
        context.beginPath()
        context.ellipse(ring.x, ring.y, radius, radius * RING_FLATTEN, 0, 0, TAU)
        context.stroke()
      }
      if (ring.click && t < 0.2) {
        context.globalAlpha = 0.5 * (1 - t / 0.2)
        context.beginPath()
        context.ellipse(ring.x, ring.y, 5, 5 * RING_FLATTEN, 0, 0, TAU)
        context.fill()
      }
    }
    for (const spark of sim.sparks) {
      context.globalAlpha = 0.8 * (1 - spark.age / spark.life)
      context.beginPath()
      context.arc(spark.x, spark.y, 1.3, 0, TAU)
      context.fill()
    }
    context.globalAlpha = 1
  }
}
