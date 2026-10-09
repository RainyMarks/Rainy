/** Pane size limits shared by pointer resizing, keyboard resizing and rendering of stored sizes. */

/** Keyboard resize step in pixels. */
export const RESIZE_STEP = 16

/**
 * @param width Viewport width.
 * @param agentVisible Whether the AI pane is shown.
 * @returns Whether the file pane becomes an overlay instead of a column.
 */
export function isNarrow(width: number, agentVisible: boolean): boolean {
  return width <= 720 || (width <= 1100 && agentVisible)
}

/** @param value Requested width. @returns The file pane width, 180–360px. */
export function clampSidebar(value: number): number {
  return Math.min(360, Math.max(180, value))
}

/**
 * @param value Requested width.
 * @param viewportWidth Viewport width.
 * @param leftWidth Width taken by the file pane column.
 * @returns The AI pane width: at least 300px while leaving at least 300px for the editor.
 */
export function clampAgent(value: number, viewportWidth: number, leftWidth: number): number {
  return Math.max(300, Math.min(viewportWidth - leftWidth - 300, value))
}

/**
 * @param value Requested height.
 * @param viewportHeight Viewport height.
 * @returns The bottom panel height: at least 120px and at most the viewport height minus 230px.
 */
export function clampBottom(value: number, viewportHeight: number): number {
  return Math.max(120, Math.min(viewportHeight - 230, value))
}
