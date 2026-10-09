/** Main window geometry restored across launches. */
import { readFile } from 'node:fs/promises'
import { renameSync, rmSync, writeFileSync } from 'node:fs'
import { randomUUID } from 'node:crypto'
import { dirname, join } from 'node:path'
import { z } from 'zod'

const rectangle = z.object({
  x: z.number().int(), y: z.number().int(), width: z.number().int().positive(), height: z.number().int().positive(),
}).strict()
const stateSchema = z.object({ version: z.literal(1), bounds: rectangle, maximized: z.boolean() }).strict()

/** Screen rectangle in device-independent pixels. */
export type WindowRectangle = z.infer<typeof rectangle>
/** Last normal (unmaximized) bounds and whether the window was maximized. */
export type WindowState = z.infer<typeof stateSchema>

/** A window keeps this much of its top edge on a display, enough to grab and move it. */
const VISIBLE_GRIP = { width: 120, height: 40 }

/**
 * @param path - private window state record.
 * @returns the saved state, or undefined when it is missing or unreadable, which keeps the default placement.
 */
export async function readWindowState(path: string): Promise<WindowState | undefined> {
  try { return stateSchema.parse(JSON.parse(await readFile(path, 'utf8'))) }
  catch (_unusableState) { return undefined } // A first launch or damaged record opens at the default size.
}

/**
 * Persist geometry synchronously, because the window is closing while the process exits.
 * @param path - private window state record.
 * @param state - geometry to restore.
 */
export function writeWindowState(path: string, state: WindowState): void {
  const temporary = join(dirname(path), `.window-state-${randomUUID()}.tmp`)
  try {
    writeFileSync(temporary, JSON.stringify(state) + '\n', { mode: 0o600 })
    renameSync(temporary, path)
  } finally { rmSync(temporary, { force: true }) }
}

/** Window width and height in device-independent pixels. */
export interface WindowSize {
  width: number
  height: number
}

/**
 * Place saved bounds on a connected display, shrinking them to its work area when the display became smaller.
 * @param state - saved geometry.
 * @param workAreas - work areas of the connected displays.
 * @param minimum - smallest supported window size.
 * @returns bounds to open with, or undefined when no display still shows the window's top edge.
 */
export function restoredBounds(state: WindowState, workAreas: readonly WindowRectangle[], minimum: WindowSize):
  WindowRectangle | undefined {
  const { bounds } = state
  const area = workAreas.find((work) => {
    const sharedWidth = Math.min(bounds.x + bounds.width, work.x + work.width) - Math.max(bounds.x, work.x)
    return sharedWidth >= VISIBLE_GRIP.width
      && bounds.y >= work.y - VISIBLE_GRIP.height / 2 && bounds.y <= work.y + work.height - VISIBLE_GRIP.height
  })
  if (area === undefined) return undefined
  const width = Math.max(minimum.width, Math.min(bounds.width, area.width))
  const height = Math.max(minimum.height, Math.min(bounds.height, area.height))
  return {
    x: Math.min(Math.max(bounds.x, area.x), area.x + area.width - width),
    y: Math.min(Math.max(bounds.y, area.y), area.y + area.height - height),
    width,
    height,
  }
}

/**
 * Choose the opening geometry: the saved placement while a display still shows it, else the preferred size centered on the
 * primary work area. A work area too small for the preferred size opens maximized over 90% normal bounds.
 * @param state - saved geometry, if any.
 * @param displays - connected work areas and the primary one.
 * @param preferred - default window size.
 * @param minimum - smallest supported window size.
 * @returns normal bounds and whether to maximize them.
 */
export function initialPlacement(
  state: WindowState | undefined,
  displays: { workAreas: readonly WindowRectangle[]; primary: WindowRectangle },
  preferred: WindowSize,
  minimum: WindowSize,
): { bounds: WindowRectangle; maximized: boolean } {
  const restored = state === undefined ? undefined : restoredBounds(state, displays.workAreas, minimum)
  if (state !== undefined && restored !== undefined) return { bounds: restored, maximized: state.maximized }
  const area = displays.primary
  const fits = preferred.width <= area.width && preferred.height <= area.height
  const width = fits ? preferred.width : Math.max(minimum.width, Math.round(area.width * 0.9))
  const height = fits ? preferred.height : Math.max(minimum.height, Math.round(area.height * 0.9))
  const x = area.x + Math.max(0, Math.round((area.width - width) / 2))
  const y = area.y + Math.max(0, Math.round((area.height - height) / 2))
  return { bounds: { x, y, width, height }, maximized: !fits }
}
