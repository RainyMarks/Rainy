/** The project map picks boxes per row for the pane it is shown in, and fits the drawing within the zoom limits. */
import { describe, expect, it } from 'vitest'
import { fitTransform, GAP_X, layoutForPane, MIN_SCALE, NODE_WIDTH } from '../../src/renderer/map/layout.ts'

const rowWidth = (count: number): number => count * NODE_WIDTH + (count - 1) * GAP_X
const loose = (count: number): string[] => Array.from({ length: count }, (_, index) => `part${index}`)
/** One box importing eight others: a layer of one above a layer of eight. */
const hub = ['app', ...loose(8)]
const hubEdges = loose(8).map(to => ({ from: 'app', to, weight: 1 }))

describe('layoutForPane', () => {
  it('wraps a narrow pane into two columns, readable once fitted', () => {
    const pane = { width: 400, height: 600 }
    const layout = layoutForPane(hub, hubEdges, pane)
    expect(layout.width).toBe(rowWidth(2))
    expect(fitTransform(pane, layout).k).toBeCloseTo((400 - 80) / rowWidth(2))
  })

  it('stacks boxes in one column when the pane is narrow and tall', () => {
    expect(layoutForPane(loose(4), [], { width: 300, height: 1200 }).width).toBe(NODE_WIDTH)
  })

  it('keeps long rows in a wide, short pane', () => {
    expect(layoutForPane(loose(8), [], { width: 2000, height: 260 }).width).toBe(rowWidth(8))
  })

  it('uses the default rows before the pane is measured', () => {
    expect(layoutForPane(loose(8), [], { width: 0, height: 0 }).width).toBe(rowWidth(6))
    expect(layoutForPane([], [], { width: 400, height: 400 }).nodes.size).toBe(0)
  })
})

describe('fitTransform', () => {
  it('centres a small drawing at a little over full size', () => {
    expect(fitTransform({ width: 1000, height: 800 }, { width: 200, height: 100 })).toEqual({ k: 1.15, x: 385, y: 342.5 })
  })

  it('stops at the smallest zoom and keeps the top of a tall drawing in view', () => {
    expect(fitTransform({ width: 300, height: 200 }, { width: 2000, height: 2000 })).toEqual({ k: MIN_SCALE, x: -50, y: 40 })
  })
})
