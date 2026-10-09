/** Window-wide shortcuts of the Windows profile. Keys handled first by the editor or an open dialog are left to them. */
import { useEffect, useRef } from 'react'

/** Key caps of the quick-open shortcut. */
export const QUICK_OPEN_KEYS: readonly string[] = ['Ctrl', '+', 'Alt', '+', 'P']

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
}

/**
 * @param event Key press.
 * @returns The bound command, if any.
 */
export function shortcutCommand(event: Pick<KeyboardEvent, 'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey' | 'isComposing' | 'code'>): keyof ShortcutHandlers | undefined {
  if (!event.ctrlKey || event.shiftKey || event.metaKey || event.isComposing) return undefined
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
    const keydown = (event: KeyboardEvent): void => {
      if (event.defaultPrevented) return
      const command = shortcutCommand(event)
      if (command === undefined || (command !== 'settings' && modalOpen())) return
      event.preventDefault()
      latest.current[command]()
    }
    window.addEventListener('keydown', keydown)
    return () => { window.removeEventListener('keydown', keydown) }
  }, [])
}
