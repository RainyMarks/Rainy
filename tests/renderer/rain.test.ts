/** The rain behind empty views: density follows the view, drops fall along the mark's slant and ring where they land, and a click ripples. */
import { expect, it } from 'vitest'
import { AREA_PER_DROP, MAX_DROPS, MAX_SPLASHES, RAIN_SLANT, RainSim } from '../../src/renderer/app/rain.ts'

/** A repeatable stand-in for Math.random. */
function seeded(seed = 7): () => number {
  let state = seed
  return () => {
    state = (state * 1103515245 + 12345) % 2147483648
    return state / 2147483648
  }
}

it('keeps one drop per area up to a limit, and none in an empty view', () => {
  const sim = new RainSim(seeded())
  sim.resize(650, 400)
  expect(sim.drops).toHaveLength(Math.round((650 * 400) / AREA_PER_DROP))
  sim.resize(4000, 3000)
  expect(sim.drops).toHaveLength(MAX_DROPS)
  sim.resize(0, 0)
  expect(sim.drops).toHaveLength(0)
  sim.step(0.05)
  expect(sim.rings).toHaveLength(0)
})

it('moves each drop down and to the left along the slant of the mark', () => {
  const sim = new RainSim(seeded())
  sim.resize(800, 600)
  const before = sim.drops.map(drop => ({ ...drop }))
  sim.step(0.01)
  for (const [index, drop] of sim.drops.entries()) {
    const start = before[index]!
    // A drop that reached its end was replaced by a new one at the top.
    if (drop.y < start.y) continue
    expect(drop.y).toBeGreaterThan(start.y)
    expect((start.x - drop.x) / (drop.y - start.y)).toBeCloseTo(Math.tan(RAIN_SLANT), 6)
  }
})

it('leaves a ring where a landing drop ends, and the ring fades away', () => {
  const sim = new RainSim(seeded(3))
  sim.resize(800, 600)
  for (let frame = 0; frame < 30 && sim.rings.length === 0; frame++) sim.step(0.03)
  const ring = sim.rings[0]!
  expect(ring.click).toBe(false)
  expect(ring.y).toBeGreaterThan(0)
  expect(ring.y).toBeLessThanOrEqual(600)
  const life = ring.life
  sim.step(life)
  expect(sim.rings).not.toContain(ring)
})

it('ripples where the user clicks, pushes the drops the wave passes and keeps a few ripples at once', () => {
  const sim = new RainSim(seeded())
  sim.resize(800, 600)
  sim.splash(400, 300)
  const ripple = sim.rings.find(ring => ring.click)!
  expect(ripple).toMatchObject({ x: 400, y: 300, waves: 3 })
  expect(sim.sparks.length).toBeGreaterThan(0)
  sim.step(0.3)
  // The outer wave eases out to the ripple's radius over its life.
  const reached = ripple.radius * (1 - (1 - 0.3 / ripple.life) ** 3)
  const front = { x: 400 + reached, y: 300, depth: 2, speed: 0, end: 600, lands: false }
  const moved = sim.disturbance(front)
  expect(moved.dx).toBeGreaterThan(0)
  expect(moved.boost).toBeGreaterThan(0)
  expect(sim.disturbance({ ...front, x: 20, y: 20 }).boost).toBe(0)
  for (let count = 0; count < MAX_SPLASHES + 3; count++) sim.splash(100, 100)
  expect(sim.rings.filter(ring => ring.click)).toHaveLength(MAX_SPLASHES)
  sim.step(2)
  expect(sim.rings.filter(ring => ring.click)).toHaveLength(0)
  expect(sim.sparks).toHaveLength(0)
})
