/** Sample the host palette after its theme presenter has updated the document. */
import type { CtfColors } from './ctf-protocol.ts'

/**
 * Parse semantic token values through the owning browser's CSS color implementation.
 * @param document - host document after theme presentation.
 * @returns the complete palette, or undefined while its tokens are unavailable.
 */
export function readCtfColors(document: Document): CtfColors | undefined {
  const view = document.defaultView
  if (view === null) return undefined
  const bodyStyle = view.getComputedStyle(document.body)
  const probe = document.createElement('span')
  probe.setAttribute('aria-hidden', 'true')
  probe.style.cssText = 'position:fixed;visibility:hidden;pointer-events:none;width:0;height:0'
  document.body.append(probe)
  try {
    const color = (token: string): string => {
      const value = bodyStyle.getPropertyValue(token).trim()
      if (!value) return ''
      probe.style.color = ''
      probe.style.color = value
      if (!probe.style.color) return ''
      const parsed = view.getComputedStyle(probe).color.trim()
      return parsed.includes('var(') ? '' : parsed
    }
    const colors: CtfColors = {
      background: color('--dsw-alias-bg-base'),
      panel: color('--dsw-alias-bg-layer-1'),
      input: color('--dsw-alias-bg-layer-1'),
      hover: color('--dsw-alias-interactive-bg-hover'),
      text: color('--dsw-alias-label-primary'),
      secondary: color('--dsw-alias-label-secondary'),
      border: color('--dsw-alias-border-l1'),
      accent: color('--dsw-alias-button-primary-fill'),
      accentText: color('--dsw-alias-label-primary-foreground'),
      error: color('--dsw-alias-state-error-primary'),
      warning: color('--dsw-alias-state-warn-primary'),
      success: color('--dsw-alias-state-success-primary'),
    }
    return Object.values(colors).some(value => value === '') ? undefined : colors
  } finally {
    probe.remove()
  }
}
