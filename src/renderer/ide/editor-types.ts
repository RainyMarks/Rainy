/** Interface between the IDE and the CodeMirror editor and xterm terminal in `src/editor`. */

/** Source models use Host file URIs; snippets and unmounted recovery use isolated untitled URIs. */
export interface EditorDocument {
  readonly path: string
  readonly uri: string
  readonly language: string
  readonly text: string
  readonly readOnly: boolean
}

/** Persistable text position and scroll offset. */
export interface EditorView {
  readonly line: number
  readonly column: number
  readonly top: number
  readonly left: number
}

/** Explicitly selected source sent through the normal chat submission path. */
export interface EditorSelection {
  readonly path: string
  readonly text: string
  readonly language: string
  readonly startLine: number
  readonly startColumn: number
  readonly endLine: number
  readonly endColumn: number
}

/** A language-server diagnostic for an open document. */
export interface EditorProblem {
  readonly path: string
  readonly line: number
  readonly column: number
  readonly endLine: number
  readonly endColumn: number
  readonly message: string
  readonly severity: 'error' | 'warning' | 'info' | 'hint'
  readonly source: string
}

/** Host theme values already resolved by the owning theme presenter. */
export interface EditorAppearance {
  readonly dark: boolean
  readonly fontSize: number
  /** CSS font family list of code. */
  readonly fontFamily?: string
  readonly background?: string
  readonly foreground?: string
}

/** Locale-owned labels for the editor's context menu and rename prompt. */
export interface EditorActionLabels {
  readonly save: string
  readonly format: string
  readonly sendSelection: string
  readonly toggleBreakpoint: string
  readonly gotoDefinition: string
  readonly findReferences: string
  readonly rename: string
  /** Context-menu item and name of the Ctrl+K prompt. */
  readonly aiEdit: string
  readonly aiEditPlaceholder: string
  readonly aiEditGenerate: string
  readonly aiEditGenerating: string
  readonly aiEditStop: string
  readonly aiEditAccept: string
  readonly aiEditReject: string
  /** Status line once a result is shown in place. */
  readonly aiEditReview: string
  readonly aiEditUnchanged: string
  readonly aiEditUnavailable: string
  /** Buttons on each change while the agent's changes are reviewed. */
  readonly reviewKeep: string
  readonly reviewUndo: string
  readonly reviewKeepTitle: string
  readonly reviewUndoTitle: string
  /** Interface language of the editor's own panels (search, go to line, folding). */
  readonly locale: 'zh' | 'en'
}

/** One workspace's language-service address and source root. */
export interface EditorWorkspace {
  readonly id: string
  readonly path: string
  readonly title: string
  /** Mounted folders share one project identity while each language server owns its root. */
  readonly roots?: readonly { readonly rootId: string; readonly path: string; readonly title: string }[] | undefined
  readonly pythonPath?: string | undefined
  readonly compileCommandsDirectory?: string | undefined
}

/** Text around the cursor for a ghost-text suggestion. */
export interface EditorSuggestRequest {
  readonly path: string
  readonly language: string
  readonly prefix: string
  readonly suffix: string
}

/** A selection to rewrite, with its surroundings and the instruction typed after Ctrl+K. */
export interface EditorRewriteRequest {
  readonly path: string
  readonly language: string
  readonly before: string
  readonly selection: string
  readonly after: string
  readonly instruction: string
}

/** Editor events; the IDE model owns buffers and saves, the editor never writes files. */
export interface EditorCallbacks {
  readonly change: (path: string, text: string) => void
  readonly view: (path: string, view: EditorView) => void
  readonly selection: (selection: EditorSelection | undefined) => void
  readonly problems: (problems: readonly EditorProblem[]) => void
  readonly languageState: (language: string, phase: 'starting' | 'ready' | 'error', message?: string) => void
  readonly open: (uri: string, line?: number, column?: number) => Promise<EditorDocument | undefined>
  readonly read: (uri: string) => Promise<EditorDocument | undefined>
  readonly prepareEdit: (uri: string) => Promise<EditorDocument | undefined>
  readonly save: () => void
  readonly format: () => void
  readonly sendSelection: () => void
  readonly breakpoint: (path: string, line: number) => void
  /** Ghost-text suggestion at the cursor, or '' for none; the signal aborts when the user keeps typing. */
  readonly suggest?: ((request: EditorSuggestRequest, signal: AbortSignal) => Promise<string>) | undefined
  /** Replacement for a selection; `progress` receives the text so far. Rejects with a message to show. */
  readonly rewrite?: ((request: EditorRewriteRequest, progress: (text: string) => void, signal: AbortSignal) => Promise<string>) | undefined
  /** First visible line while the user scrolls, with a fraction for the part of it scrolled past; drives preview sync. */
  readonly scroll?: ((path: string, line: number) => void) | undefined
  /** A review kept or undid a change: the comparison text now, and how many changes are left. */
  readonly review?: ((path: string, state: { readonly original: string; readonly remaining: number }) => void) | undefined
}

/** A retained editor: one view per open document, plus its language-server connections. */
export interface EditorInstance {
  /** @param workspace Next workspace. @returns Completion after previous language clients stop. */
  setWorkspace(workspace: EditorWorkspace): Promise<void>
  /** @param documents Complete open-model set. Existing text changes retain undo history where possible. */
  updateDocuments(documents: readonly EditorDocument[]): void
  /** @param path Selected model. @param view Optional restored cursor and scroll state. */
  show(path: string, view?: EditorView): void
  /**
   * @param path Selected model.
   * @param original Read-only comparison text.
   * @param editable Whether the modified buffer remains editable.
   * @param review Give each change keep and undo buttons, and report them through `review`.
   */
  showDiff(path: string, original: string, editable: boolean, review?: boolean): void
  /** @param appearance Resolved host appearance. */
  setAppearance(appearance: EditorAppearance): void
  /** @param options Whether ghost-text suggestions are requested while typing. */
  setAssist(options: { readonly suggestions: boolean }): void
  /** @param labels Labels after a locale change; open panels and menus keep their text until they reopen. */
  setLabels(labels: EditorActionLabels): void
  /** @param breakpoints Persisted source line breakpoints. @param stopped Optional paused source position. */
  setBreakpoints(
    breakpoints: readonly { readonly path: string; readonly lines: readonly number[] }[],
    stopped?: { readonly path: string; readonly line: number },
  ): void
  /** @param path Source path. @param line One-based line. @param column One-based column. */
  reveal(path: string, line: number, column: number): void
  /**
   * Run an editor command on the shown document.
   * @param command One of `rainy.save`, `rainy.format`, `rainy.selection`, `rainy.aiEdit`, `rainy.nextChange`, `rainy.previousChange`,
   *   `rainy.keepAll`, `rainy.undoAll` (the last four in a diff), `find`, `replace`, `gotoLine`, `undo`, `redo`,
   *   `selectAll`, `toggleComment`, `gotoDefinition`, `findReferences`, `rename`, `formatDocument`.
   * @returns Completion; rejects for an unknown command.
   */
  action(command: string): Promise<void>
  /** Request a layout after the containing pane becomes visible. */
  layout(): void
  /** @param path Open document. @param line One-based line, possibly fractional, to place at the top without moving the cursor. */
  scrollToLine(path: string, line: number): void
  /** @returns Completion after owned language clients, models, and editors stop. */
  dispose(): Promise<void>
}

/** Interactive terminal whose data transport remains owned by the workspace controller. */
export interface EditorTerminal {
  /** @param text Terminal output, including ANSI escapes. */
  write(text: string): void
  /** Clear terminal history when the Host reports a different retained output prefix. */
  reset(): void
  /** Fit rows and columns to the visible container. */
  fit(): void
  /** Place keyboard focus in the terminal. */
  focus(): void
  /** @returns The text selected in the terminal, or '' when nothing is. */
  selection?(): string
  /** @param appearance Resolved host appearance; the terminal follows its light or dark scheme. */
  setAppearance(appearance: EditorAppearance): void
  /** Release terminal subscriptions and DOM ownership. */
  dispose(): void
}

/** Editor module entry, loaded on the first editor or terminal visit. */
export interface EditorAssets {
  readonly version: 1
  /**
   * @param container Editor mount point.
   * @param callbacks Explicit UI callbacks.
   * @param labels Localized actions.
   * @returns A ready editor.
   */
  create(container: HTMLElement, callbacks: EditorCallbacks, labels: EditorActionLabels): Promise<EditorInstance>
  /**
   * @param container Terminal mount point.
   * @param data User input callback.
   * @param resize Measured terminal size callback.
   * @returns A mounted terminal.
   */
  terminal(
    container: HTMLElement,
    data: (text: string) => void,
    resize: (cols: number, rows: number) => void,
  ): EditorTerminal
}
