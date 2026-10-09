/** Messages exchanged only with Rainy's authenticated local workbench frame. */

/** Independent drafts never substitute for a Session id. */
export type CtfContext = { readonly kind: 'session'; readonly id: string } | { readonly kind: 'standalone' }

/** Parsed semantic colors from the host's current CSS tokens. */
export interface CtfColors {
  readonly background: string
  readonly panel: string
  readonly input: string
  readonly hover: string
  readonly text: string
  readonly secondary: string
  readonly border: string
  readonly accent: string
  readonly accentText: string
  readonly error: string
  readonly warning: string
  readonly success: string
}

/** Resolved host appearance for the isolated workbench document. */
export interface CtfAppearance {
  readonly dark: boolean
  readonly fontSize: number
  readonly codeFontSize: number
  readonly locale: 'zh' | 'en'
  /** Omitted before host tokens are available; the frame retains its fallback palette. */
  readonly colors?: CtfColors
}

/** Host configuration applied serially by the frame. */
export interface CtfConfiguration {
  readonly context: CtfContext
  readonly appearance: CtfAppearance
  readonly visible: boolean
}

/** A flush succeeds only after current edits reach durable Host storage. */
export interface CtfFlushResult { readonly ok: boolean; readonly error?: string }

/** Host-to-frame transport messages. */
export type CtfHostMessage = (CtfConfiguration & { readonly type: 'rainy:configure'; readonly revision: number })
  | { readonly type: 'rainy:flush'; readonly id: string }

/** Desktop saves through the root workbench owner before stopping the Host. */
export interface DesktopWorkbenchBridge {
  /** @param flush - saves current edits. @returns listener disposer. */
  onFlush(flush: () => Promise<CtfFlushResult>): () => void
}
