/** xterm.js terminal for the IDE bottom panel. */
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import type { EditorAppearance, EditorTerminal } from '../renderer/ide/editor-types.ts'

/** Light and dark ANSI palettes. */
const terminalThemes = {
  light: { background: '#ffffff', foreground: '#1f1f1f', cursor: '#1f1f1f', cursorAccent: '#ffffff', selectionBackground: '#add6ff', selectionInactiveBackground: '#e5ebf1',
    black: '#000000', red: '#cd3131', green: '#107c10', yellow: '#949800', blue: '#0451a5', magenta: '#bc05bc', cyan: '#0598bc', white: '#555555',
    brightBlack: '#666666', brightRed: '#cd3131', brightGreen: '#14ce14', brightYellow: '#b5ba00', brightBlue: '#0451a5',
    brightMagenta: '#bc05bc', brightCyan: '#0598bc', brightWhite: '#a5a5a5' },
  dark: { background: '#111111', foreground: '#cccccc', cursor: '#cccccc', cursorAccent: '#111111', selectionBackground: '#264f78', selectionInactiveBackground: '#2d2d2d',
    black: '#000000', red: '#cd3131', green: '#0dbc79', yellow: '#e5e510', blue: '#2472c8', magenta: '#bc3fbc', cyan: '#11a8cd', white: '#e5e5e5',
    brightBlack: '#666666', brightRed: '#f14c4c', brightGreen: '#23d18b', brightYellow: '#f5f543', brightBlue: '#3b8eea',
    brightMagenta: '#d670d6', brightCyan: '#29b8db', brightWhite: '#e5e5e5' },
}

/** Code font until the page supplies its own; the same families as `--rainy-font-family-code`. */
const DEFAULT_FONT_FAMILY = "'SF Mono', 'JetBrains Mono', 'Fira Code', Consolas, 'Liberation Mono', Menlo, Courier, monospace"

/**
 * Mount a terminal.
 * @param container Mount point; the terminal fits it whenever it resizes.
 * @param data Receives user input.
 * @param resize Receives the fitted column and row counts.
 * @returns The terminal handle.
 */
export function createTerminal(container: HTMLElement, data: (text: string) => void, resize: (cols: number, rows: number) => void): EditorTerminal {
  const terminal = new Terminal({ fontSize: 13, cursorBlink: true, allowProposedApi: false, convertEol: false,
    fontFamily: DEFAULT_FONT_FAMILY, theme: terminalThemes.light })
  const fit = new FitAddon()
  terminal.loadAddon(fit)
  terminal.open(container)
  const onData = terminal.onData(data)
  const onResize = terminal.onResize((size) => { resize(size.cols, size.rows) })
  const fitVisible = (): void => { if (container.clientWidth > 0 && container.clientHeight > 0) fit.fit() }
  const observer = new ResizeObserver(fitVisible)
  observer.observe(container)
  fitVisible()
  const setAppearance = (appearance: EditorAppearance): void => {
    const palette = appearance.dark ? terminalThemes.dark : terminalThemes.light
    // The page's surface and text colors follow its color scheme; the ANSI colors stay.
    const background = appearance.background ?? palette.background
    const foreground = appearance.foreground ?? palette.foreground
    terminal.options.theme = { ...palette, background, foreground, cursor: foreground, cursorAccent: background }
    const fontFamily = appearance.fontFamily ?? DEFAULT_FONT_FAMILY
    if (terminal.options.fontSize === appearance.fontSize && terminal.options.fontFamily === fontFamily) return
    terminal.options.fontSize = appearance.fontSize
    terminal.options.fontFamily = fontFamily
    fitVisible()
  }
  return { write: (text) => { terminal.write(text) }, reset: () => { terminal.reset() }, fit: fitVisible, focus: () => { terminal.focus() },
    selection: () => terminal.getSelection(),
    setAppearance, dispose: () => { observer.disconnect(); onData.dispose(); onResize.dispose(); terminal.dispose() } }
}
