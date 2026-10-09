/** CodeMirror 6 editors, language-server clients and the xterm terminal behind the IDE's `EditorAssets` interface. */
import { Annotation, Compartment, EditorState, RangeSet, StateEffect, StateField, type Extension } from '@codemirror/state'
import {
  crosshairCursor, Decoration, drawSelection, dropCursor, EditorView, gutter, GutterMarker, highlightActiveLine, highlightActiveLineGutter,
  highlightSpecialChars, keymap, lineNumbers, rectangularSelection, showPanel, type Command, type DecorationSet, type Panel,
} from '@codemirror/view'
import { defaultKeymap, history, historyKeymap, indentWithTab, redo, selectAll, toggleComment, undo } from '@codemirror/commands'
import { bracketMatching, foldGutter, foldKeymap, indentOnInput, indentUnit } from '@codemirror/language'
import { gotoLine, highlightSelectionMatches, openSearchPanel, search, searchKeymap } from '@codemirror/search'
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete'
import { lintKeymap } from '@codemirror/lint'
import { unifiedMergeView } from '@codemirror/merge'
import { findReferences, jumpToDefinition, LSPPlugin } from '@codemirror/lsp-client'
import type {
  EditorActionLabels, EditorAppearance, EditorAssets, EditorCallbacks, EditorDocument, EditorInstance, EditorProblem, EditorView as EditorViewState,
  EditorWorkspace,
} from '../renderer/ide/editor-types.ts'
import { languageKey, loadLanguage, lspLanguageId, serverLanguage } from './languages.ts'
import { chinesePhrases } from './phrases.ts'
import { LanguageConnection, type LanguageHost, type LspDiagnostic, type LspPosition, type LspTextEdit } from './lsp.ts'
import { createTerminal } from './terminal.ts'
import { editorHighlighting, editorTheme } from './theme.ts'
import { uriInside, uriKey } from './uri.ts'
import './editor.css'

/** Marks transactions that apply text the IDE model already holds, so they are not reported back as edits. */
const external = Annotation.define<boolean>()

const languageSlot = new Compartment()
const readOnlySlot = new Compartment()
const lspSlot = new Compartment()
const diffSlot = new Compartment()
const darkSlot = new Compartment()
const indentSlot = new Compartment()

// ── Breakpoints and the paused line ──

const setBreakpoints = StateEffect.define<{ lines: readonly number[]; stopped: number | undefined }>()

class BreakpointMarker extends GutterMarker {
  override toDOM(): Node {
    const dot = document.createElement('span')
    dot.className = 'rainy-breakpoint'
    return dot
  }
}
const breakpointMarker = new BreakpointMarker()
const stoppedLine = Decoration.line({ class: 'rainy-stopped-line' })

const breakpointField = StateField.define<{ markers: RangeSet<GutterMarker>; stopped: DecorationSet }>({
  create: () => ({ markers: RangeSet.empty, stopped: Decoration.none }),
  update(value, transaction) {
    let next = transaction.docChanged ? { markers: value.markers.map(transaction.changes), stopped: value.stopped.map(transaction.changes) } : value
    for (const effect of transaction.effects) {
      if (!effect.is(setBreakpoints)) continue
      const doc = transaction.state.doc
      const lines = [...new Set(effect.value.lines)].filter(line => line >= 1 && line <= doc.lines).sort((left, right) => left - right)
      const stopped = effect.value.stopped
      next = {
        markers: RangeSet.of(lines.map(line => breakpointMarker.range(doc.line(line).from))),
        stopped: stopped !== undefined && stopped >= 1 && stopped <= doc.lines ? Decoration.set([stoppedLine.range(doc.line(stopped).from)]) : Decoration.none,
      }
    }
    return next
  },
  provide: field => EditorView.decorations.from(field, value => value.stopped),
})

// ── Rename prompt ──

interface RenameRequest { word: string; label: string; done(name: string): void }
const toggleRename = StateEffect.define<RenameRequest | null>()

const renameField = StateField.define<RenameRequest | null>({
  create: () => null,
  update(value, transaction) {
    for (const effect of transaction.effects) if (effect.is(toggleRename)) return effect.value
    return value
  },
  provide: field => showPanel.from(field, request => request === null ? null : (view): Panel => {
    const dom = document.createElement('form')
    dom.className = 'rainy-rename-panel'
    const label = document.createElement('label')
    label.textContent = request.label
    const input = document.createElement('input')
    input.className = 'cm-textfield'
    input.value = request.word
    input.setAttribute('aria-label', request.label)
    label.append(input)
    dom.append(label)
    const close = (): void => { view.dispatch({ effects: toggleRename.of(null) }); view.focus() }
    dom.addEventListener('submit', (event) => {
      event.preventDefault()
      const name = input.value.trim()
      close()
      if (name !== '' && name !== request.word) request.done(name)
    })
    input.addEventListener('keydown', (event) => { if (event.key === 'Escape') { event.preventDefault(); close() } })
    return { dom, top: true, mount: () => { input.focus(); input.select() } }
  }),
})

// ── Helpers ──

function positionOf(state: EditorState, line: number, column: number): number {
  const target = state.doc.line(Math.min(Math.max(1, line), state.doc.lines))
  return target.from + Math.min(Math.max(0, column - 1), target.length)
}

/** Offset of an LSP position in raw text with LF or CRLF line ends. */
function offsetIn(text: string, position: LspPosition): number {
  let offset = 0
  for (let line = 0; line < position.line; line++) {
    const end = text.indexOf('\n', offset)
    if (end < 0) return text.length
    offset = end + 1
  }
  const lineEnd = text.indexOf('\n', offset)
  const length = (lineEnd < 0 ? text.length : lineEnd) - offset - (lineEnd > 0 && text[lineEnd - 1] === '\r' ? 1 : 0)
  return offset + Math.min(position.character, Math.max(0, length))
}

/** Apply LSP edits to raw text, last edit first so earlier offsets stay valid. */
function applyTextEdits(text: string, edits: readonly LspTextEdit[]): string {
  const ranges = edits.map(edit => ({ from: offsetIn(text, edit.range.start), to: offsetIn(text, edit.range.end), insert: edit.newText }))
    .sort((left, right) => right.from - left.from)
  let result = text
  for (const range of ranges) result = result.slice(0, range.from) + range.insert + result.slice(range.to)
  return result
}

/** Indentation the file already uses: a tab, or the smallest run of leading spaces among its first indented lines. */
function detectIndent(text: string): string {
  let smallest = 0
  let seen = 0
  for (const line of text.split('\n', 2000)) {
    if (line.startsWith('\t')) return '\t'
    const spaces = /^( +)\S/.exec(line)?.[1]?.length
    if (spaces === undefined) continue
    if (smallest === 0 || spaces < smallest) smallest = spaces
    if (++seen >= 20) break
  }
  return smallest === 2 || smallest === 4 || smallest === 8 ? ' '.repeat(smallest) : '    '
}

/** Minimal replacement that turns `before` into `after`, so cursors and folds outside the change survive. */
function minimalChange(before: string, after: string): { from: number; to: number; insert: string } {
  let start = 0
  const limit = Math.min(before.length, after.length)
  while (start < limit && before.charCodeAt(start) === after.charCodeAt(start)) start++
  let end = 0
  while (end < limit - start && before.charCodeAt(before.length - 1 - end) === after.charCodeAt(after.length - 1 - end)) end++
  return { from: start, to: before.length - end, insert: after.slice(start, after.length - end) }
}

const severities: Record<number, EditorProblem['severity']> = { 1: 'error', 2: 'warning', 3: 'info', 4: 'hint' }

interface WorkspaceEdit {
  changes?: Record<string, LspTextEdit[]>
  documentChanges?: ({ textDocument: { uri: string }; edits: LspTextEdit[] } | { kind: string })[]
}

/** One open document and its editor view. */
interface OpenDocument {
  document: EditorDocument
  key: string | undefined
  view: EditorView
  host: HTMLDivElement
  /** Connection whose plugin the view holds. */
  connection: LanguageConnection | undefined
  /** Comparison text while the document shows as a diff. */
  diff: { original: string; editable: boolean } | undefined
  /** The view state the IDE saved has been applied. */
  restored: boolean
}

class WorkspaceEditor implements EditorInstance, LanguageHost {
  private readonly documents = new Map<string, OpenDocument>()
  private readonly connections = new Map<string, LanguageConnection>()
  private readonly diagnosticsByUri = new Map<string, { uri: string; diagnostics: readonly LspDiagnostic[] }>()
  private readonly breakpoints = new Map<string, readonly number[]>()
  private stopped: { path: string; line: number } | undefined
  private workspace: EditorWorkspace | undefined
  private activePath: string | undefined
  private dark = false
  private disposed = false
  private menu: HTMLElement | undefined
  private menuCleanup: (() => void) | undefined
  private readonly viewTimers = new Map<string, number>()

  constructor(private readonly container: HTMLElement, private readonly callbacks: EditorCallbacks, private readonly labels: EditorActionLabels) {
    container.classList.add('rainy-editor')
  }

  // ── EditorInstance ──

  async setWorkspace(workspace: EditorWorkspace): Promise<void> {
    if (this.disposed) return
    const previous = this.workspace
    if (previous !== undefined && previous.id === workspace.id && previous.pythonPath === workspace.pythonPath
      && previous.compileCommandsDirectory === workspace.compileCommandsDirectory && JSON.stringify(previous.roots) === JSON.stringify(workspace.roots)) return
    this.closeConnections()
    this.workspace = workspace
    for (const open of this.documents.values()) this.attachLanguageServer(open)
  }

  updateDocuments(documents: readonly EditorDocument[]): void {
    if (this.disposed) return
    const paths = new Set(documents.map(document => document.path))
    for (const [path, open] of this.documents) if (!paths.has(path)) this.close(path, open)
    for (const document of documents) this.ensure(document)
    this.publishProblems()
  }

  show(path: string, view?: EditorViewState): void {
    const open = this.activate(path)
    if (open === undefined) return
    if (open.diff !== undefined) {
      open.diff = undefined
      open.view.dispatch({ effects: [diffSlot.reconfigure([]), readOnlySlot.reconfigure(this.readOnly(open))] })
    }
    if (view !== undefined && !open.restored) {
      open.restored = true
      const anchor = positionOf(open.view.state, view.line, view.column)
      open.view.dispatch({ selection: { anchor }, annotations: external.of(true) })
      requestAnimationFrame(() => {
        open.view.scrollDOM.scrollTop = view.top
        open.view.scrollDOM.scrollLeft = view.left
      })
    }
  }

  showDiff(path: string, original: string, editable: boolean): void {
    const open = this.activate(path)
    if (open === undefined) return
    if (open.diff?.original === original && open.diff.editable === editable) return
    open.diff = { original, editable }
    open.view.dispatch({ effects: [
      diffSlot.reconfigure(unifiedMergeView({ original, mergeControls: false, gutter: true, highlightChanges: true, syntaxHighlightDeletions: true })),
      readOnlySlot.reconfigure(this.readOnly(open)),
    ] })
  }

  setAppearance(appearance: EditorAppearance): void {
    const style = this.container.style
    style.setProperty('--rainy-editor-font-size', `${appearance.fontSize}px`)
    if (appearance.background !== undefined) style.setProperty('--rainy-editor-background', appearance.background)
    else style.removeProperty('--rainy-editor-background')
    if (appearance.foreground !== undefined) style.setProperty('--rainy-editor-foreground', appearance.foreground)
    else style.removeProperty('--rainy-editor-foreground')
    if (appearance.dark === this.dark) return
    this.dark = appearance.dark
    for (const open of this.documents.values()) open.view.dispatch({ effects: darkSlot.reconfigure(EditorView.darkTheme.of(this.dark)) })
  }

  setBreakpoints(breakpoints: readonly { readonly path: string; readonly lines: readonly number[] }[], stopped?: { readonly path: string; readonly line: number }): void {
    this.breakpoints.clear()
    for (const source of breakpoints) this.breakpoints.set(source.path, source.lines)
    this.stopped = stopped === undefined ? undefined : { path: stopped.path, line: stopped.line }
    for (const [path, open] of this.documents) open.view.dispatch({ effects: setBreakpoints.of(this.breakpointsFor(path)) })
  }

  reveal(path: string, line: number, column: number): void {
    const open = path === this.activePath ? this.documents.get(path) : this.activate(path)
    if (open === undefined) return
    const anchor = positionOf(open.view.state, line, column)
    open.view.dispatch({ selection: { anchor }, effects: EditorView.scrollIntoView(anchor, { y: 'center' }) })
    open.view.focus()
  }

  action(command: string): Promise<void> {
    const view = this.activePath === undefined ? undefined : this.documents.get(this.activePath)?.view
    const run = (target: Command): void => { if (view !== undefined) { target(view); view.focus() } }
    switch (command) {
      case 'rainy.save': this.callbacks.save(); break
      case 'rainy.format':
      case 'formatDocument': this.callbacks.format(); break
      case 'rainy.selection': this.callbacks.sendSelection(); break
      case 'find':
      case 'replace': run(openSearchPanel); break
      case 'gotoLine': run(gotoLine); break
      case 'undo': run(undo); break
      case 'redo': run(redo); break
      case 'selectAll': run(selectAll); break
      case 'toggleComment': run(toggleComment); break
      case 'gotoDefinition': run(jumpToDefinition); break
      case 'findReferences': run(findReferences); break
      case 'rename': run(this.renameCommand); break
      default: return Promise.reject(new Error(`Unknown editor action: ${command}`))
    }
    return Promise.resolve()
  }

  layout(): void {
    const open = this.activePath === undefined ? undefined : this.documents.get(this.activePath)
    open?.view.requestMeasure()
  }

  async dispose(): Promise<void> {
    this.disposed = true
    this.closeMenu()
    this.closeConnections()
    for (const [path, open] of this.documents) this.close(path, open)
    this.container.replaceChildren()
    this.container.classList.remove('rainy-editor')
  }

  // ── LanguageHost ──

  async display(uri: string): Promise<EditorView | null> {
    const key = uriKey(uri)
    const existing = [...this.documents.values()].find(open => uriKey(open.document.uri) === key)
    if (existing !== undefined) { this.activate(existing.document.path); return existing.view }
    const document = await this.callbacks.open(uri)
    if (document === undefined || this.disposed) return null
    const open = this.ensure(document)
    this.activate(document.path)
    return open.view
  }

  async read(uri: string): Promise<string | undefined> {
    return (await this.callbacks.read(uri))?.text
  }

  diagnostics(uri: string, diagnostics: readonly LspDiagnostic[]): void {
    if (diagnostics.length === 0) this.diagnosticsByUri.delete(uriKey(uri))
    else this.diagnosticsByUri.set(uriKey(uri), { uri, diagnostics })
    this.publishProblems()
  }

  // ── Documents ──

  private readOnly(open: Pick<OpenDocument, 'document' | 'diff'>): Extension {
    return EditorState.readOnly.of(open.document.readOnly || (open.diff !== undefined && !open.diff.editable))
  }

  private breakpointsFor(path: string): { lines: readonly number[]; stopped: number | undefined } {
    return { lines: this.breakpoints.get(path) ?? [], stopped: this.stopped?.path === path ? this.stopped.line : undefined }
  }

  private activate(path: string): OpenDocument | undefined {
    const open = this.documents.get(path)
    if (open === undefined) return undefined
    this.closeMenu()
    if (this.activePath !== path) {
      const previous = this.activePath === undefined ? undefined : this.documents.get(this.activePath)
      if (previous !== undefined) previous.host.hidden = true
      this.activePath = path
      open.host.hidden = false
      open.view.requestMeasure()
      this.publishSelection(open.document, open.view)
    }
    return open
  }

  private ensure(document: EditorDocument): OpenDocument {
    const existing = this.documents.get(document.path)
    if (existing !== undefined) {
      const previous = existing.document
      existing.document = document
      const current = existing.view.state.doc.toString()
      const effects: StateEffect<unknown>[] = []
      if (previous.readOnly !== document.readOnly) effects.push(readOnlySlot.reconfigure(this.readOnly(existing)))
      if (current !== document.text || effects.length > 0) {
        existing.view.dispatch({
          ...(current === document.text ? {} : { changes: minimalChange(current, document.text) }),
          effects, annotations: external.of(true),
        })
      }
      const key = languageKey(document.path, document.language)
      if (key !== existing.key || uriKey(previous.uri) !== uriKey(document.uri)) {
        existing.key = key
        this.loadLanguageInto(existing)
        this.attachLanguageServer(existing)
      } else if (existing.connection === undefined) this.attachLanguageServer(existing)
      return existing
    }

    const host = window.document.createElement('div')
    host.className = 'rainy-editor-surface'
    host.hidden = true
    this.container.append(host)
    const pending: Omit<OpenDocument, 'view'> = { document, key: languageKey(document.path, document.language), host, connection: undefined, diff: undefined, restored: false }
    const view = new EditorView({ parent: host, state: EditorState.create({ doc: document.text, extensions: this.extensions(pending) }) })
    const open: OpenDocument = Object.assign(pending, { view })
    open.view.dispatch({ effects: setBreakpoints.of(this.breakpointsFor(document.path)) })
    view.scrollDOM.addEventListener('scroll', () => { this.scheduleView(open.document.path, view) }, { passive: true })
    open.view.dom.addEventListener('contextmenu', (event) => { this.openMenu(open, event) })
    this.documents.set(document.path, open)
    this.loadLanguageInto(open)
    this.attachLanguageServer(open)
    return open
  }

  private extensions(open: Omit<OpenDocument, 'view'>): Extension {
    const path = (): string => open.document.path
    return [
      lineNumbers(),
      gutter({
        class: 'rainy-breakpoint-gutter',
        // Every line needs a cell to receive the click that sets a breakpoint.
        renderEmptyElements: true,
        markers: view => view.state.field(breakpointField).markers,
        initialSpacer: () => breakpointMarker,
        domEventHandlers: {
          mousedown: (view, line) => { this.callbacks.breakpoint(path(), view.state.doc.lineAt(line.from).number); return true },
        },
      }),
      foldGutter(),
      highlightActiveLineGutter(),
      highlightSpecialChars(),
      history(),
      drawSelection(),
      dropCursor(),
      EditorState.allowMultipleSelections.of(true),
      indentOnInput(),
      bracketMatching(),
      closeBrackets(),
      autocompletion(),
      rectangularSelection(),
      crosshairCursor(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      search({ top: true }),
      this.labels.locale === 'zh' ? EditorState.phrases.of(chinesePhrases) : [],
      breakpointField,
      renameField,
      editorTheme,
      editorHighlighting,
      keymap.of([
        { key: 'Mod-s', run: () => { this.callbacks.save(); return true }, preventDefault: true },
        { key: 'Shift-Alt-f', run: () => { this.callbacks.format(); return true }, preventDefault: true },
        { key: 'Mod-g', run: gotoLine, preventDefault: true },
        { key: 'Mod-h', run: openSearchPanel, preventDefault: true },
        { key: 'F2', run: this.renameCommand, preventDefault: true },
        ...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...historyKeymap, ...foldKeymap, ...completionKeymap, ...lintKeymap, indentWithTab,
      ]),
      EditorView.updateListener.of((update) => {
        if (update.docChanged && !update.transactions.every(transaction => transaction.annotation(external) === true)) {
          this.callbacks.change(open.document.path, update.state.doc.toString())
        }
        if ((update.selectionSet || update.docChanged) && this.activePath === open.document.path) {
          this.publishSelection(open.document, update.view)
          this.scheduleView(open.document.path, update.view)
        }
      }),
      languageSlot.of([]),
      readOnlySlot.of(this.readOnly(open)),
      lspSlot.of([]),
      diffSlot.of([]),
      darkSlot.of(EditorView.darkTheme.of(this.dark)),
      indentSlot.of(indentUnit.of(detectIndent(open.document.text))),
    ]
  }

  private loadLanguageInto(open: OpenDocument): void {
    const key = open.key
    void loadLanguage(key).then((language) => {
      if (this.documents.get(open.document.path) !== open || open.key !== key) return
      open.view.dispatch({ effects: languageSlot.reconfigure(language) })
    })
  }

  private close(path: string, open: OpenDocument): void {
    window.clearTimeout(this.viewTimers.get(path))
    this.viewTimers.delete(path)
    open.view.destroy()
    open.host.remove()
    this.documents.delete(path)
    this.diagnosticsByUri.delete(uriKey(open.document.uri))
    if (this.activePath === path) this.activePath = undefined
  }

  // ── Language servers ──

  private rootFor(uri: string): { rootId: string; path: string; title: string } | undefined {
    const workspace = this.workspace
    if (workspace === undefined) return undefined
    const roots = workspace.roots ?? [{ rootId: 'primary', path: workspace.path, title: workspace.title }]
    return [...roots].sort((left, right) => right.path.length - left.path.length).find(root => uriInside(uri, root.path))
  }

  private attachLanguageServer(open: OpenDocument): void {
    const workspace = this.workspace
    const language = serverLanguage(open.key)
    const languageId = lspLanguageId(open.key)
    const root = open.document.uri.startsWith('file:') ? this.rootFor(open.document.uri) : undefined
    if (workspace === undefined || language === undefined || languageId === undefined || root === undefined) {
      if (open.connection !== undefined) { open.connection = undefined; open.view.dispatch({ effects: lspSlot.reconfigure([]) }) }
      return
    }
    const identity = `${root.rootId}:${language}`
    let connection = this.connections.get(identity)
    if (connection === undefined) {
      const created = new LanguageConnection(
        { workspaceId: workspace.id, rootId: root.rootId, root: root.path, title: root.title, language, pythonPath: workspace.pythonPath },
        this,
        (message) => {
          if (this.connections.get(identity) !== created) return
          this.connections.delete(identity)
          for (const item of this.documents.values()) {
            if (item.connection === created) { item.connection = undefined; item.view.dispatch({ effects: lspSlot.reconfigure([]) }) }
          }
          this.callbacks.languageState(identity, 'error', message)
        },
      )
      connection = created
      this.connections.set(identity, created)
      this.callbacks.languageState(identity, 'starting')
      created.ready.then(() => {
        if (this.connections.get(identity) === created) this.callbacks.languageState(identity, 'ready')
      }, () => undefined)
    }
    if (open.connection === connection) return
    open.connection = connection
    open.view.dispatch({ effects: lspSlot.reconfigure(connection.plugin(open.document.uri, languageId)) })
  }

  private closeConnections(): void {
    for (const open of this.documents.values()) {
      if (open.connection === undefined) continue
      open.connection = undefined
      open.view.dispatch({ effects: lspSlot.reconfigure([]) })
    }
    for (const connection of this.connections.values()) connection.close()
    this.connections.clear()
    this.diagnosticsByUri.clear()
    this.publishProblems()
  }

  /** Rename the symbol at the cursor in every file the server names, opening closed files as unsaved buffers. */
  private readonly renameCommand: Command = (view) => {
    const plugin = LSPPlugin.get(view)
    const word = view.state.wordAt(view.state.selection.main.head)
    if (plugin === null || word === null) return false
    const position = plugin.toPosition(word.from)
    view.dispatch({ effects: toggleRename.of({
      word: view.state.sliceDoc(word.from, word.to),
      label: this.labels.rename,
      done: (newName) => { void this.rename(plugin, position, newName) },
    }) })
    return true
  }

  private async rename(plugin: LSPPlugin, position: LspPosition, newName: string): Promise<void> {
    try {
      plugin.client.sync()
      const edit = await plugin.client.request<object, WorkspaceEdit | null>('textDocument/rename', { textDocument: { uri: plugin.uri }, position, newName })
      if (edit === null || this.disposed) return
      const byUri = new Map<string, LspTextEdit[]>()
      for (const [uri, edits] of Object.entries(edit.changes ?? {})) byUri.set(uri, edits)
      for (const change of edit.documentChanges ?? []) if ('textDocument' in change) byUri.set(change.textDocument.uri, change.edits)
      const targets: { uri: string; edits: LspTextEdit[]; open: OpenDocument | undefined; document: EditorDocument | undefined }[] = []
      for (const [uri, edits] of byUri) {
        const open = [...this.documents.values()].find(item => uriKey(item.document.uri) === uriKey(uri))
        const document = open === undefined ? await this.callbacks.prepareEdit(uri) : open.document
        if (document === undefined || document.readOnly) throw new Error(`${this.labels.rename}: ${uri}`)
        targets.push({ uri, edits, open, document })
      }
      for (const target of targets) {
        if (target.open !== undefined) {
          const view = target.open.view
          const lspPlugin = LSPPlugin.get(view)
          view.dispatch({
            changes: target.edits.map(item => ({
              from: lspPlugin === null ? offsetIn(view.state.doc.toString(), item.range.start) : lspPlugin.unsyncedChanges.mapPos(lspPlugin.fromPosition(item.range.start, lspPlugin.syncedDoc)),
              to: lspPlugin === null ? offsetIn(view.state.doc.toString(), item.range.end) : lspPlugin.unsyncedChanges.mapPos(lspPlugin.fromPosition(item.range.end, lspPlugin.syncedDoc)),
              insert: item.newText,
            })),
            userEvent: 'rename',
          })
        } else if (target.document !== undefined) {
          this.callbacks.change(target.document.path, applyTextEdits(target.document.text, target.edits))
        }
      }
    } catch (error) {
      plugin.reportError(this.labels.rename, error)
    }
  }

  // ── Reporting ──

  private publishSelection(document: EditorDocument, view: EditorView): void {
    const selection = view.state.selection.main
    if (selection.empty) { this.callbacks.selection(undefined); return }
    const doc = view.state.doc
    const start = doc.lineAt(selection.from)
    const end = doc.lineAt(selection.to)
    this.callbacks.selection({
      path: document.path, text: doc.sliceString(selection.from, selection.to), language: document.language,
      startLine: start.number, startColumn: selection.from - start.from + 1, endLine: end.number, endColumn: selection.to - end.from + 1,
    })
  }

  private scheduleView(path: string, view: EditorView): void {
    if (this.viewTimers.has(path)) return
    this.viewTimers.set(path, window.setTimeout(() => {
      this.viewTimers.delete(path)
      if (this.documents.get(path)?.view !== view) return
      const head = view.state.selection.main.head
      const line = view.state.doc.lineAt(head)
      this.callbacks.view(path, { line: line.number, column: head - line.from + 1, top: view.scrollDOM.scrollTop, left: view.scrollDOM.scrollLeft })
    }, 150))
  }

  private publishProblems(): void {
    const problems: EditorProblem[] = []
    for (const open of this.documents.values()) {
      const entry = this.diagnosticsByUri.get(uriKey(open.document.uri))
      if (entry === undefined) continue
      for (const diagnostic of entry.diagnostics) {
        problems.push({
          path: open.document.path, line: diagnostic.range.start.line + 1, column: diagnostic.range.start.character + 1,
          endLine: diagnostic.range.end.line + 1, endColumn: diagnostic.range.end.character + 1, message: diagnostic.message,
          severity: severities[diagnostic.severity ?? 1] ?? 'error', source: diagnostic.source ?? '',
        })
      }
    }
    this.callbacks.problems(problems)
  }

  // ── Context menu ──

  private openMenu(open: OpenDocument, event: MouseEvent): void {
    event.preventDefault()
    this.closeMenu()
    const view = open.view
    const position = view.posAtCoords({ x: event.clientX, y: event.clientY })
    if (position !== null && view.state.selection.ranges.every(range => position < range.from || position > range.to)) {
      view.dispatch({ selection: { anchor: position } })
    }
    const hasSelection = !view.state.selection.main.empty
    const language = LSPPlugin.get(view) !== null
    const items: { label: string; enabled: boolean; run(): void }[] = [
      { label: this.labels.sendSelection, enabled: hasSelection, run: () => { this.callbacks.sendSelection() } },
      { label: this.labels.gotoDefinition, enabled: language, run: () => { jumpToDefinition(view) } },
      { label: this.labels.findReferences, enabled: language, run: () => { findReferences(view) } },
      { label: this.labels.rename, enabled: language && !open.document.readOnly, run: () => { this.renameCommand(view) } },
      { label: this.labels.format, enabled: !open.document.readOnly, run: () => { this.callbacks.format() } },
      { label: this.labels.toggleBreakpoint, enabled: true, run: () => { this.callbacks.breakpoint(open.document.path, view.state.doc.lineAt(view.state.selection.main.head).number) } },
    ]
    const menu = document.createElement('div')
    menu.className = 'rainy-editor-menu'
    menu.setAttribute('role', 'menu')
    for (const item of items) {
      const button = document.createElement('button')
      button.type = 'button'
      button.setAttribute('role', 'menuitem')
      button.textContent = item.label
      button.disabled = !item.enabled
      // Focus returns to the editor first, so a command that opens a prompt keeps the focus it moves.
      button.addEventListener('click', () => { this.closeMenu(); view.focus(); item.run() })
      menu.append(button)
    }
    document.body.append(menu)
    const width = menu.offsetWidth
    const height = menu.offsetHeight
    menu.style.left = `${Math.min(event.clientX, window.innerWidth - width - 4)}px`
    menu.style.top = `${Math.min(event.clientY, window.innerHeight - height - 4)}px`
    this.menu = menu
    const dismiss = (closeEvent: Event): void => {
      if (closeEvent instanceof KeyboardEvent && closeEvent.key !== 'Escape') return
      if (closeEvent.type === 'pointerdown' && closeEvent.target instanceof Node && menu.contains(closeEvent.target)) return
      this.closeMenu()
    }
    const listeners = ['pointerdown', 'keydown', 'resize'] as const
    for (const name of listeners) window.addEventListener(name, dismiss, true)
    this.menuCleanup = () => { for (const name of listeners) window.removeEventListener(name, dismiss, true) }
    menu.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus()
  }

  private closeMenu(): void {
    this.menuCleanup?.()
    this.menuCleanup = undefined
    this.menu?.remove()
    this.menu = undefined
  }
}

/** Editor and terminal factories for the IDE. */
export const assets: EditorAssets = {
  version: 1,
  create: async (container, callbacks, labels) => new WorkspaceEditor(container, callbacks, labels),
  terminal: createTerminal,
}
