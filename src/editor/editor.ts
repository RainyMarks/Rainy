/** Offline Monaco, language-client, and PTY rendering assets loaded by the Rainy workspace. */
import * as monaco from 'monaco-editor'
import { MonacoVscodeApiWrapper } from 'monaco-languageclient/vscodeApiWrapper'
import type { MonacoVscodeApiConfig } from 'monaco-languageclient/vscodeApiWrapper'
import { useWorkerFactory, Worker as WorkerDefinition } from 'monaco-languageclient/workerFactory'
import { MonacoLanguageClient } from 'monaco-languageclient'
import { CloseAction, ErrorAction } from 'vscode-languageclient/browser'
import { WebSocketMessageReader, WebSocketMessageWriter, toSocket } from 'vscode-ws-jsonrpc'
import { RegisteredFileSystemProvider, RegisteredMemoryFile, registerFileSystemOverlay } from '@codingame/monaco-vscode-files-service-override'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import '@codingame/monaco-vscode-python-default-extension'
import '@codingame/monaco-vscode-typescript-basics-default-extension'
import '@codingame/monaco-vscode-cpp-default-extension'
import '@codingame/monaco-vscode-json-default-extension'
import '@codingame/monaco-vscode-theme-defaults-default-extension'
import './editor.css'
import { canonicalEditorUri, containsEditorUri } from './editor-uri.ts'
import type {
  EditorActionLabels, EditorAppearance, EditorAssets, EditorCallbacks, EditorDocument, EditorInstance,
  EditorProblem, EditorTerminal, EditorView, EditorWorkspace,
} from '../renderer/ide/editor-types.ts'

const instances = new Set<WorkspaceEditor>()
const languageIds = new Set(['python', 'typescript', 'javascript', 'c', 'cpp'])
let initialization: Promise<void> | undefined

function ownerFor(uri: string): WorkspaceEditor | undefined {
  return [...instances].find(instance => instance.contains(uri))
}

function cancelled(token: { readonly isCancellationRequested: boolean }): boolean {
  return token.isCancellationRequested
}

class WorkspaceFiles extends RegisteredFileSystemProvider {
  private readonly registered = new Set<string>()
  constructor() { super(false) }

  register(document: EditorDocument): void {
    const uri = canonicalEditorUri(document.uri)
    const key = uri.toString()
    if (this.registered.has(key)) return
    this.registerFile(new RegisteredMemoryFile(uri, document.text))
    this.registered.add(key)
  }

  override async stat(uri: monaco.Uri) {
    const canonical = canonicalEditorUri(uri)
    try { return await super.stat(canonical) } catch (error) {
      const document = await ownerFor(canonical.toString())?.read(canonical.toString())
      if (document === undefined) throw error
      this.register(document)
      return super.stat(canonical)
    }
  }

  override async readFile(uri: monaco.Uri): Promise<Uint8Array> {
    const canonical = canonicalEditorUri(uri)
    const document = await ownerFor(canonical.toString())?.read(canonical.toString())
    if (document !== undefined) { this.register(document); return new TextEncoder().encode(document.text) }
    return super.readFile(canonical)
  }
}

const files = new WorkspaceFiles()

function initialize(): Promise<void> {
  initialization ??= (async () => {
    registerFileSystemOverlay(10, files)
    const options: MonacoVscodeApiConfig = {
      $type: 'extended',
      viewsConfig: { $type: 'EditorService', openEditorFunc: async (reference, options) => {
        const model = reference.object.textEditorModel
        const owner = ownerFor(model.uri.toString())
        if (owner === undefined) return undefined
        const selection = options !== undefined && 'selection' in options && typeof options.selection === 'object' && options.selection !== null
          ? options.selection : undefined
        const line = selection !== undefined && 'startLineNumber' in selection && typeof selection.startLineNumber === 'number' ? selection.startLineNumber : undefined
        const column = selection !== undefined && 'startColumn' in selection && typeof selection.startColumn === 'number' ? selection.startColumn : undefined
        await owner.open(model.uri.toString(), line, column)
        return owner.normalEditor
      } },
      userConfiguration: { json: JSON.stringify({
        'editor.semanticHighlighting.enabled': true, 'editor.wordBasedSuggestions': 'matchingDocuments',
        'files.autoSave': 'off', 'workbench.colorTheme': 'Default Dark Modern',
      }) },
      advanced: { loadThemes: true, enableExtHostWorker: false },
      monacoWorkerFactory: () => {
        useWorkerFactory({ workerLoaders: {
          editorWorkerService: () => new WorkerDefinition(new URL('./editor.worker.js', import.meta.url), { type: 'module' }),
          TextMateWorker: () => new WorkerDefinition(new URL('./textmate.worker.js', import.meta.url), { type: 'module' }),
        } })
      },
    }
    await new MonacoVscodeApiWrapper(options).start()
  })()
  return initialization
}

interface ClientConnection {
  readonly socket: WebSocket
  readonly client: MonacoLanguageClient
  readonly started: Promise<void>
}

async function stopConnection(connection: ClientConnection): Promise<void> {
  try { await connection.started; await connection.client.stop() }
  catch (error) { void error } // A failed startup has no live protocol session to shut down.
  finally { connection.socket.close(1000) }
}

class WorkspaceEditor implements EditorInstance {
  readonly normalEditor: monaco.editor.IStandaloneCodeEditor
  private readonly diffEditor: monaco.editor.IStandaloneDiffEditor
  private readonly normalContainer: HTMLDivElement
  private readonly diffContainer: HTMLDivElement
  private readonly models = new Map<string, { document: EditorDocument; model: monaco.editor.ITextModel; listener: monaco.IDisposable }>()
  private readonly clients = new Map<string, ClientConnection>()
  private readonly decorations = new Map<string, string[]>()
  private readonly subscriptions: monaco.IDisposable[] = []
  private workspace: EditorWorkspace | undefined
  private activePath: string | undefined
  private originalModel: monaco.editor.ITextModel | undefined
  private updating = false
  private disposed = false
  private generation = 0
  private diffVisible = false
  private workspaceChange: Promise<void> = Promise.resolve()

  constructor(private readonly container: HTMLElement, private readonly callbacks: EditorCallbacks, labels: EditorActionLabels) {
    this.normalContainer = document.createElement('div')
    this.diffContainer = document.createElement('div')
    this.normalContainer.className = 'rainy-monaco-surface'
    this.diffContainer.className = 'rainy-monaco-surface'
    this.diffContainer.hidden = true
    container.append(this.normalContainer, this.diffContainer)
    this.normalEditor = monaco.editor.create(this.normalContainer, {
      automaticLayout: true, model: null, minimap: { enabled: false }, scrollBeyondLastLine: false,
      glyphMargin: true, fontSize: 13, tabSize: 2, padding: { top: 8, bottom: 8 },
    })
    this.diffEditor = monaco.editor.createDiffEditor(this.diffContainer, {
      automaticLayout: true, renderSideBySide: true, originalEditable: false, minimap: { enabled: false },
      scrollBeyondLastLine: false, glyphMargin: true,
    })
    for (const editor of [this.normalEditor, this.diffEditor.getModifiedEditor()]) {
      this.subscriptions.push(editor.onDidChangeCursorSelection((event) => {
        const item = this.activePath === undefined ? undefined : this.models.get(this.activePath)
        if (item === undefined || editor.getModel() !== item.model) return
        const selected = event.selection
        const text = item.model.getValueInRange(selected)
        this.callbacks.selection(text === '' ? undefined : { path: item.document.path, text, language: item.document.language,
          startLine: selected.startLineNumber, startColumn: selected.startColumn,
          endLine: selected.endLineNumber, endColumn: selected.endColumn })
        this.publishView(editor)
      }))
      this.subscriptions.push(editor.onDidScrollChange(() => { this.publishView(editor) }))
      this.subscriptions.push(editor.onMouseDown((event) => {
        if (event.target.type === monaco.editor.MouseTargetType.GUTTER_GLYPH_MARGIN && this.activePath !== undefined) {
          this.callbacks.breakpoint(this.activePath, event.target.position.lineNumber)
        }
      }))
      this.subscriptions.push(editor.addAction({ id: 'rainy.save', label: labels.save,
        keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS], run: () => { this.callbacks.save() } }))
      this.subscriptions.push(editor.addAction({ id: 'rainy.format', label: labels.format,
        keybindings: [monaco.KeyMod.Shift | monaco.KeyMod.Alt | monaco.KeyCode.KeyF], run: () => { this.callbacks.format() } }))
      this.subscriptions.push(editor.addAction({ id: 'rainy.selection', label: labels.sendSelection,
        contextMenuGroupId: 'navigation', contextMenuOrder: 3, run: () => { this.callbacks.sendSelection() } }))
    }
    this.subscriptions.push(monaco.editor.onDidChangeMarkers(() => { this.publishProblems() }))
    instances.add(this)
  }

  contains(uri: string): boolean {
    if (!this.workspace) return false
    return this.roots().some(root => this.containsRoot(uri, root.path))
  }

  private roots(): readonly { rootId: string; path: string; title: string }[] {
    return this.workspace?.roots ?? (this.workspace === undefined ? [] : [{ rootId: 'primary', path: this.workspace.path, title: this.workspace.title }])
  }

  private containsRoot(uri: string, path: string): boolean {
    return containsEditorUri(uri, path)
  }

  async read(uri: string): Promise<EditorDocument | undefined> { return this.callbacks.read(uri) }

  async open(uri: string, line?: number, column?: number): Promise<void> {
    const document = await this.callbacks.open(uri, line, column)
    if (document === undefined || this.disposed) return
    this.ensureModel(document)
    this.show(document.path)
    if (line !== undefined) this.reveal(document.path, line, column ?? 1)
  }

  async setWorkspace(workspace: EditorWorkspace): Promise<void> {
    const changed = this.workspaceChange.then(async () => {
      if (this.disposed || (this.workspace?.id === workspace.id && this.workspace.pythonPath === workspace.pythonPath
        && this.workspace.compileCommandsDirectory === workspace.compileCommandsDirectory
        && JSON.stringify(this.workspace.roots) === JSON.stringify(workspace.roots))) return
      this.generation++
      const previous = [...this.clients.values()]
      this.clients.clear()
      await Promise.all(previous.map(stopConnection))
      this.workspace = workspace
    })
    this.workspaceChange = changed
    await changed
  }

  private ensureModel(document: EditorDocument): void {
    const uri = canonicalEditorUri(document.uri)
    const previous = this.models.get(document.path)
    if (previous?.model.uri.toString() === uri.toString()) {
      previous.document = document
      if (previous.model.getValue() !== document.text) {
        this.updating = true
        previous.model.pushEditOperations([], [{ range: previous.model.getFullModelRange(), text: document.text }], () => null)
        this.updating = false
      }
      monaco.editor.setModelLanguage(previous.model, document.language)
      return
    }
    if (previous) { previous.listener.dispose(); previous.model.dispose() }
    files.register(document)
    const model = monaco.editor.getModel(uri) ?? monaco.editor.createModel(document.text, document.language, uri)
    const listener = model.onDidChangeContent(() => {
      if (!this.updating) this.callbacks.change(document.path, model.getValue())
    })
    this.models.set(document.path, { document, model, listener })
  }

  updateDocuments(documents: readonly EditorDocument[]): void {
    const paths = new Set(documents.map(item => item.path))
    for (const [path, item] of this.models) {
      if (!paths.has(path)) { item.listener.dispose(); item.model.dispose(); this.models.delete(path) }
    }
    for (const item of documents) this.ensureModel(item)
    for (const document of documents.filter(item => item.uri.startsWith('file:'))) {
      const root = [...this.roots()].sort((a, b) => b.path.length - a.path.length).find(item => this.containsRoot(document.uri, item.path))
      if (root !== undefined) void this.connect(document.language, root)
    }
  }

  private isCurrent(generation: number): boolean {
    return generation === this.generation && !this.disposed
  }

  private async connect(language: string, root: { rootId: string; path: string; title: string }): Promise<void> {
    const workspace = this.workspace
    const identity = `${root.rootId}:${language}`
    if (!workspace || !languageIds.has(language) || this.clients.has(identity) || this.disposed) return
    const generation = this.generation
    const url = new URL('/rainy/ide/lsp', location.href)
    url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
    url.searchParams.set('workspaceId', workspace.id)
    url.searchParams.set('language', language)
    url.searchParams.set('rootId', root.rootId)
    const socket = new WebSocket(url)
    const transport = toSocket(socket)
    const client = new MonacoLanguageClient({ id: `rainy-${workspace.id}-${root.rootId}-${language}`, name: `Rainy ${language}`,
      clientOptions: {
        documentSelector: [{ language, scheme: 'file', pattern: `${monaco.Uri.file(root.path).path.replace(/\/$/, '')}/**/*` }],
        workspaceFolder: { index: 0, name: root.title, uri: monaco.Uri.file(root.path) },
        initializationOptions: language === 'python' ? { pythonPath: workspace.pythonPath } : undefined,
        middleware: { provideRenameEdits: async (document, position, newName, token, next) => {
          const edit = await next(document, position, newName, token)
          if (edit === undefined || edit === null || token.isCancellationRequested) return edit
          for (const [uri] of edit.entries()) {
            const source = await this.callbacks.prepareEdit(uri.toString())
            if (source === undefined || source.readOnly || !this.isCurrent(generation)) return null
            this.ensureModel(source)
          }
          return cancelled(token) ? null : edit
        }, workspace: { configuration: async (params, token, next) => {
          const result = await next(params, token)
          if (!Array.isArray(result)) return result
          const defaults: readonly unknown[] = result
          return params.items.map((item, index) => {
            if (language !== 'python' || workspace.pythonPath === undefined) return defaults[index]
            if (item.section === 'python.pythonPath') return workspace.pythonPath
            if (item.section === 'python') {
              const value: unknown = defaults[index]
              return { ...(typeof value === 'object' && value !== null ? value : {}), pythonPath: workspace.pythonPath }
            }
            return defaults[index]
          })
        } } },
        errorHandler: { error: () => ({ action: ErrorAction.Continue }), closed: () => ({ action: CloseAction.DoNotRestart }) },
      },
      messageTransports: { reader: new WebSocketMessageReader(transport), writer: new WebSocketMessageWriter(transport) },
    })
    this.callbacks.languageState(identity, 'starting')
    const started = (async () => {
      await new Promise<void>((accept, reject) => {
        socket.addEventListener('open', () => { accept() }, { once: true })
        socket.addEventListener('error', () => { reject(new Error('Language service connection failed.')) }, { once: true })
        socket.addEventListener('close', () => { if (socket.readyState !== WebSocket.OPEN) reject(new Error('Language service connection closed.')) }, { once: true })
      })
      if (!this.isCurrent(generation)) { socket.close(1000); return }
      await client.start()
      if (language === 'python') await client.sendNotification('workspace/didChangeConfiguration', { settings: { python: { pythonPath: workspace.pythonPath } } })
      if (this.isCurrent(generation)) this.callbacks.languageState(identity, 'ready')
    })()
    this.clients.set(identity, { socket, client, started })
    try { await started } catch (error) {
      socket.close(1000)
      if (this.isCurrent(generation)) {
        this.callbacks.languageState(identity, 'error', error instanceof Error ? error.message : String(error))
      }
    }
  }

  show(path: string, view?: EditorView): void {
    const item = this.models.get(path)
    if (!item) return
    this.activePath = path
    this.diffVisible = false
    this.normalContainer.hidden = false
    this.diffContainer.hidden = true
    this.normalEditor.setModel(item.model)
    this.normalEditor.updateOptions({ readOnly: item.document.readOnly })
    if (view) {
      this.normalEditor.setPosition({ lineNumber: view.line, column: view.column })
      this.normalEditor.setScrollPosition({ scrollTop: view.top, scrollLeft: view.left })
    }
    this.layout()
  }

  showDiff(path: string, original: string, editable: boolean): void {
    const item = this.models.get(path)
    if (!item) return
    this.activePath = path
    this.diffVisible = true
    this.normalContainer.hidden = true
    this.diffContainer.hidden = false
    this.diffEditor.setModel(null)
    this.originalModel?.dispose()
    this.originalModel = monaco.editor.createModel(original, item.document.language)
    this.diffEditor.setModel({ original: this.originalModel, modified: item.model })
    this.diffEditor.updateOptions({ readOnly: !editable || item.document.readOnly })
    this.layout()
  }

  setAppearance(appearance: EditorAppearance): void {
    monaco.editor.setTheme(appearance.dark ? 'vs-dark' : 'vs')
    this.normalEditor.updateOptions({ fontSize: appearance.fontSize })
    this.diffEditor.updateOptions({ fontSize: appearance.fontSize })
  }

  setBreakpoints(
    breakpoints: readonly { readonly path: string; readonly lines: readonly number[] }[],
    stopped?: { readonly path: string; readonly line: number },
  ): void {
    for (const [path, item] of this.models) {
      const points = breakpoints.find(source => source.path === path)?.lines ?? []
      const decorations: monaco.editor.IModelDeltaDecoration[] = points.map(line => ({
        range: new monaco.Range(line, 1, line, 1), options: { isWholeLine: true, glyphMarginClassName: 'rainy-monaco-breakpoint' },
      }))
      if (stopped?.path === path) decorations.push({ range: new monaco.Range(stopped.line, 1, stopped.line, 1),
        options: { isWholeLine: true, className: 'rainy-monaco-stopped' } })
      this.decorations.set(path, item.model.deltaDecorations(this.decorations.get(path) ?? [], decorations))
    }
  }

  reveal(path: string, line: number, column: number): void {
    if (path !== this.activePath) this.show(path)
    const editor = this.diffVisible ? this.diffEditor.getModifiedEditor() : this.normalEditor
    editor.setPosition({ lineNumber: line, column })
    editor.revealLineInCenter(line)
    editor.focus()
  }

  async action(command: string): Promise<void> {
    const editor = this.diffVisible ? this.diffEditor.getModifiedEditor() : this.normalEditor
    await editor.getAction(command)?.run()
  }

  layout(): void { this.normalEditor.layout(); this.diffEditor.layout() }

  private publishView(editor: monaco.editor.ICodeEditor): void {
    const position = editor.getPosition()
    if (!this.activePath || !position) return
    this.callbacks.view(this.activePath, { line: position.lineNumber, column: position.column,
      top: editor.getScrollTop(), left: editor.getScrollLeft() })
  }

  private publishProblems(): void {
    const problems: EditorProblem[] = []
    for (const { document, model } of this.models.values()) {
      for (const marker of monaco.editor.getModelMarkers({ resource: model.uri })) {
        problems.push({ path: document.path, line: marker.startLineNumber, column: marker.startColumn,
          endLine: marker.endLineNumber, endColumn: marker.endColumn, message: marker.message, source: marker.source ?? '',
          severity: marker.severity === monaco.MarkerSeverity.Error ? 'error'
            : marker.severity === monaco.MarkerSeverity.Warning ? 'warning' : marker.severity === monaco.MarkerSeverity.Info ? 'info' : 'hint' })
      }
    }
    this.callbacks.problems(problems)
  }

  async dispose(): Promise<void> {
    this.disposed = true
    this.generation++
    instances.delete(this)
    await this.workspaceChange
    const clients = [...this.clients.values()]
    this.clients.clear()
    await Promise.all(clients.map(stopConnection))
    for (const subscription of this.subscriptions) subscription.dispose()
    this.normalEditor.dispose()
    this.diffEditor.dispose()
    this.originalModel?.dispose()
    for (const item of this.models.values()) { item.listener.dispose(); item.model.dispose() }
    this.models.clear()
    this.container.replaceChildren()
  }
}

// Monaco's vs and vs-dark palettes, so the terminal matches the editor above it.
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

function terminal(container: HTMLElement, data: (text: string) => void, resize: (cols: number, rows: number) => void): EditorTerminal {
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

const assets: EditorAssets = { version: 1, create: async (container, callbacks, labels) => {
  await initialize()
  return new WorkspaceEditor(container, callbacks, labels)
}, terminal }
window.__RAINY_EDITOR_ASSETS__ = assets
