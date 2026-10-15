/** Window-wide shortcuts of the Windows profile. Keys handled first by the editor or an open dialog are left to them. */
import { useEffect, useRef } from 'react'

/** Key caps of the quick-open shortcut. */
export const QUICK_OPEN_KEYS: readonly string[] = ['Ctrl', '+', 'Alt', '+', 'P']
/** Key caps of the command palette. */
export const PALETTE_KEYS: readonly string[] = ['Ctrl', '+', 'Shift', '+', 'P']

/** Actions bound to the window shortcuts. */
export interface ShortcutHandlers {
  /** Ctrl+Alt+P */
  readonly quickOpen: () => void
  /** Ctrl+, — runs even while the settings dialog is open, so it can close it. */
  readonly settings: () => void
  /** Ctrl+Alt+N */
  readonly newChat: () => void
  /** Ctrl+Alt+K */
  readonly searchHistory: () => void
  /** Ctrl+Alt+O */
  readonly openFolder: () => void
  /** Ctrl+` */
  readonly newTerminal: () => void
  /** Ctrl+Shift+V: switch a Markdown or SVG tab between source and preview. @returns `false` when the tab has no preview. */
  readonly togglePreview: () => boolean
  /** Ctrl+K V: show the preview beside the source. @returns `false` when the tab has no preview. */
  readonly previewToSide: () => boolean
  /** @returns Whether the active tab has a preview, so Ctrl+K starts a chord instead of reaching the page. */
  readonly canPreview: () => boolean
  /** Ctrl+Shift+P or F1 */
  readonly commandPalette: () => void
  /** Ctrl+Shift+F */
  readonly searchFiles: () => void
  /** Ctrl+Shift+H */
  readonly replaceFiles: () => void
  /** Ctrl+Shift+G */
  readonly sourceControl: () => void
  /** Ctrl+Shift+E */
  readonly explorer: () => void
}

/** Commands of single key presses; `previewChord` is the first half of Ctrl+K V. */
export type ShortcutCommand = 'quickOpen' | 'settings' | 'newChat' | 'searchHistory' | 'openFolder' | 'newTerminal' | 'togglePreview' | 'previewChord'
  | 'commandPalette' | 'searchFiles' | 'replaceFiles' | 'sourceControl' | 'explorer'

/** Milliseconds the second key of a chord may follow the first. */
const CHORD_MS = 2000

/**
 * @param event Key press.
 * @returns The bound command, if any.
 */
export function shortcutCommand(event: Pick<KeyboardEvent, 'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey' | 'isComposing' | 'code'>): ShortcutCommand | undefined {
  if (event.code === 'F1' && !event.ctrlKey && !event.altKey && !event.shiftKey && !event.metaKey) return 'commandPalette'
  if (!event.ctrlKey || event.metaKey || event.isComposing) return undefined
  if (event.shiftKey) {
    if (event.altKey) return undefined
    switch (event.code) {
      case 'KeyV': return 'togglePreview'
      case 'KeyP': return 'commandPalette'
      case 'KeyF': return 'searchFiles'
      case 'KeyH': return 'replaceFiles'
      case 'KeyG': return 'sourceControl'
      case 'KeyE': return 'explorer'
      default: return undefined
    }
  }
  if (event.altKey) {
    switch (event.code) {
      case 'KeyP': return 'quickOpen'
      case 'KeyN': return 'newChat'
      case 'KeyK': return 'searchHistory'
      case 'KeyO': return 'openFolder'
      default: return undefined
    }
  }
  switch (event.code) {
    case 'Comma': return 'settings'
    case 'Backquote': return 'newTerminal'
    case 'KeyK': return 'previewChord'
    default: return undefined
  }
}

/** @returns Whether a modal dialog is open on the page. */
export function modalOpen(): boolean {
  return document.querySelector('[role="dialog"][aria-modal="true"]') !== null
}

/**
 * Listen to the window shortcuts while mounted. Commands other than settings are ignored while a dialog is open.
 * @param handlers Current actions; read when a key is pressed.
 */
export function useGlobalShortcuts(handlers: ShortcutHandlers): void {
  const latest = useRef(handlers)
  latest.current = handlers
  useEffect(() => {
    let chord = 0
    const keydown = (event: KeyboardEvent): void => {
      if (event.defaultPrevented) return
      if (chord !== 0) {
        const started = chord
        chord = 0
        if (Date.now() - started < CHORD_MS && event.code === 'KeyV' && !event.shiftKey && !event.altKey && !event.metaKey) {
          if (latest.current.previewToSide()) event.preventDefault()
          return
        }
      }
      const command = shortcutCommand(event)
      if (command === undefined || (command !== 'settings' && modalOpen())) return
      if (command === 'previewChord') {
        if (latest.current.canPreview()) { chord = Date.now(); event.preventDefault() }
        return
      }
      // Without a preview, Ctrl+Shift+V keeps its paste-as-plain-text meaning.
      if (command === 'togglePreview') { if (latest.current.togglePreview()) event.preventDefault(); return }
      event.preventDefault()
      latest.current[command]()
    }
    window.addEventListener('keydown', keydown)
    return () => { window.removeEventListener('keydown', keydown) }
  }, [])
}
