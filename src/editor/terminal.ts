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
  dark: { background: '#1e1e1e', foreground: '#cccccc', cursor: '#cccccc', cursorAccent: '#1e1e1e', selectionBackground: '#264f78', selectionInactiveBackground: '#3a3d41',
    black: '#000000', red: '#cd3131', green: '#0dbc79', yellow: '#e5e510', blue: '#2472c8', magenta: '#bc3fbc', cyan: '#11a8cd', white: '#e5e5e5',
    brightBlack: '#666666', brightRed: '#f14c4c', brightGreen: '#23d18b', brightYellow: '#f5f543', brightBlue: '#3b8eea',
    brightMagenta: '#d670d6', brightCyan: '#29b8db', brightWhite: '#e5e5e5' },
}

/**
 * Mount a terminal.
 * @param container Mount point; the terminal fits it whenever it resizes.
 * @param data Receives user input.
 * @param resize Receives the fitted column and row counts.
 * @returns The terminal handle.
 */
export function createTerminal(container: HTMLElement, data: (text: string) => void, resize: (cols: number, rows: number) => void): EditorTerminal {
  const terminal = new Terminal({ fontSize: 13, cursorBlink: true, allowProposedApi: false, convertEol: false,
    fontFamily: "Consolas, 'Cascadia Mono', 'Courier New', monospace", theme: terminalThemes.light })
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
    terminal.options.theme = appearance.dark ? terminalThemes.dark : terminalThemes.light
    if (terminal.options.fontSize !== appearance.fontSize) { terminal.options.fontSize = appearance.fontSize; fitVisible() }
  }
  return { write: (text) => { terminal.write(text) }, reset: () => { terminal.reset() }, fit: fitVisible, focus: () => { terminal.focus() },
    setAppearance, dispose: () => { observer.disconnect(); onData.dispose(); onResize.dispose(); terminal.dispose() } }
}
