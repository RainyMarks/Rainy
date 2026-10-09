// @vitest-environment happy-dom
/** Pane size limits, narrow-window overlays and the window shortcut table. */
import { describe, expect, it } from 'vitest'
import { clampAgent, clampBottom, clampSidebar, isNarrow } from '../../src/renderer/app/layout.ts'
import { shortcutCommand } from '../../src/renderer/app/shortcuts.ts'

describe('pane limits', () => {
  it('keeps the file pane between 180 and 360 px', () => {
    expect([clampSidebar(100), clampSidebar(240), clampSidebar(900)]).toEqual([180, 240, 360])
  })

  it('keeps the AI pane at least 300 px while leaving 300 px for the editor', () => {
    expect(clampAgent(100, 1440, 240)).toBe(300)
    expect(clampAgent(2000, 1440, 240)).toBe(900)
    expect(clampAgent(400, 1440, 240)).toBe(400)
  })

  it('keeps the bottom panel between 120 px and the window height minus 230 px', () => {
    expect([clampBottom(50, 900), clampBottom(300, 900), clampBottom(900, 900)]).toEqual([120, 300, 670])
  })

  it('turns the file pane into an overlay below 720 px, or below 1100 px with the AI pane shown', () => {
    expect([isNarrow(700, false), isNarrow(950, false), isNarrow(950, true), isNarrow(1200, true)]).toEqual([true, false, true, false])
  })
})

describe('window shortcuts', () => {
  const key = (code: string, modifiers: Partial<Record<'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey' | 'isComposing', boolean>> = {}) =>
    shortcutCommand({ code, ctrlKey: true, altKey: false, shiftKey: false, metaKey: false, isComposing: false, ...modifiers })

  it('maps the Windows profile bindings', () => {
    expect([key('KeyP', { altKey: true }), key('KeyN', { altKey: true }), key('KeyK', { altKey: true }), key('KeyO', { altKey: true }),
      key('Comma'), key('Backquote')]).toEqual(['quickOpen', 'newChat', 'searchHistory', 'openFolder', 'settings', 'newTerminal'])
  })

  it('ignores other modifiers, plain keys and keys typed during composition', () => {
    expect([key('KeyP'), key('KeyP', { altKey: true, shiftKey: true }), key('Comma', { ctrlKey: false }),
      key('KeyN', { altKey: true, isComposing: true }), key('KeyS')]).toEqual([undefined, undefined, undefined, undefined, undefined])
  })
})
