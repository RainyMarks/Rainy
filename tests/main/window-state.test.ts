import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { afterEach, describe, expect, it } from 'vitest'
import { initialPlacement, readWindowState, restoredBounds, writeWindowState } from '../src/window-state.ts'

const directories: string[] = []
afterEach(async () => { await Promise.all(directories.splice(0).map(path => rm(path, { recursive: true, force: true }))) })

const primary = { x: 0, y: 0, width: 1920, height: 1040 }
const secondary = { x: 1920, y: 0, width: 2560, height: 1400 }
const minimum = { width: 950, height: 650 }

describe('main window geometry', () => {
  it('restores bounds that still sit on a connected display', () => {
    const state = { version: 1 as const, bounds: { x: 2100, y: 80, width: 1600, height: 1000 }, maximized: true }
    expect(restoredBounds(state, [primary, secondary], minimum)).toEqual(state.bounds)
  })

  it('shrinks and moves a window into a smaller work area', () => {
    const state = { version: 1 as const, bounds: { x: 900, y: 300, width: 2400, height: 1300 }, maximized: false }
    expect(restoredBounds(state, [primary], minimum)).toEqual({ x: 0, y: 0, width: 1920, height: 1040 })
  })

  it('falls back to the default placement when the saved display was disconnected', () => {
    const state = { version: 1 as const, bounds: { x: 2100, y: 80, width: 1600, height: 1000 }, maximized: false }
    expect(restoredBounds(state, [primary], minimum)).toBeUndefined()
    // Only a sliver of the top edge would remain reachable on the primary display.
    expect(restoredBounds({ ...state, bounds: { ...state.bounds, x: 1850 } }, [primary], minimum)).toBeUndefined()
  })

  it('centers the preferred size, or opens maximized when the primary work area cannot hold it', () => {
    const preferred = { width: 1380, height: 920 }
    expect(initialPlacement(undefined, { workAreas: [primary], primary }, preferred, minimum))
      .toEqual({ bounds: { x: 270, y: 60, width: 1380, height: 920 }, maximized: false })
    const laptop = { x: 48, y: 0, width: 1280, height: 752 }
    expect(initialPlacement(undefined, { workAreas: [laptop], primary: laptop }, preferred, minimum))
      .toEqual({ bounds: { x: 112, y: 38, width: 1152, height: 677 }, maximized: true })
  })

  it('prefers a restorable saved placement, including its maximized state', () => {
    const state = { version: 1 as const, bounds: { x: 2100, y: 80, width: 1600, height: 1000 }, maximized: true }
    const displays = { workAreas: [primary, secondary], primary }
    expect(initialPlacement(state, displays, { width: 1380, height: 920 }, minimum)).toEqual({ bounds: state.bounds, maximized: true })
    expect(initialPlacement(state, { workAreas: [primary], primary }, { width: 1380, height: 920 }, minimum))
      .toEqual({ bounds: { x: 270, y: 60, width: 1380, height: 920 }, maximized: false })
  })

  it('round-trips the record and ignores damaged files', async () => {
    const directory = await mkdtemp(join(tmpdir(), 'rainy-window-state-'))
    directories.push(directory)
    const path = join(directory, 'window-state.json')
    expect(await readWindowState(path)).toBeUndefined()
    const state = { version: 1 as const, bounds: { x: 10, y: 20, width: 1200, height: 800 }, maximized: false }
    writeWindowState(path, state)
    expect(await readWindowState(path)).toEqual(state)
    await writeFile(path, '{"version":1,"bounds":{"x":"left"}}')
    expect(await readWindowState(path)).toBeUndefined()
  })
})
