/** Windows caption geometry shared by the carrier and its isolated preload. */
export const CAPTION_HEIGHT = 40
export const CAPTION_FALLBACK_WIDTH = 138

/** Accept only bounded renderer-sampled RGB colors across the native IPC boundary. */
export function captionColors(value: unknown): { color: string; symbolColor: string } | undefined {
  if (value === null || typeof value !== 'object' || !('color' in value) || !('symbolColor' in value)) return
  if (typeof value.color !== 'string' || typeof value.symbolColor !== 'string'
    || !/^#[0-9a-f]{6}$/i.test(value.color) || !/^#[0-9a-f]{6}$/i.test(value.symbolColor)) return
  return { color: value.color, symbolColor: value.symbolColor }
}
