/** Language-server connections over the Host's `/rainy/ide/lsp` WebSocket: one per project root and server language. */
import { Text, type ChangeSet, type Extension } from '@codemirror/state'
import { keymap, type EditorView } from '@codemirror/view'
import {
  findReferencesKeymap, hoverTooltips, jumpToDefinitionKeymap, LSPClient, LSPPlugin, serverCompletion, serverDiagnostics, signatureHelp,
  Workspace, type Transport, type WorkspaceFile,
} from '@codemirror/lsp-client'
import { directoryUri, uriKey } from './uri.ts'

/** LSP position. */
export interface LspPosition { line: number; character: number }
/** LSP range. */
export interface LspRange { start: LspPosition; end: LspPosition }
/** LSP diagnostic fields the editor reports. */
export interface LspDiagnostic { range: LspRange; severity?: number; message: string; source?: string }
/** LSP text edit. */
export interface LspTextEdit { range: LspRange; newText: string }

/** What a connection needs from the editor that owns it. */
export interface LanguageHost {
  /** Open a file in the editor and return its view; `null` when it cannot be shown. */
  display(uri: string): Promise<EditorView | null>
  /** Text of a file that has no view, for reference listings. */
  read(uri: string): Promise<string | undefined>
  /** The server published diagnostics for a document. */
  diagnostics(uri: string, diagnostics: readonly LspDiagnostic[]): void
}

/** Where a connection runs. */
export interface LanguageTarget {
  workspaceId: string
  rootId: string
  root: string
  title: string
  language: 'python' | 'javascript' | 'typescript' | 'c' | 'cpp'
  pythonPath?: string | undefined
}

/** Milliseconds before an LSP request is abandoned; the first pyright or clangd answers on a cold project can be slow. */
const REQUEST_TIMEOUT_MS = 15_000

class ViewFile implements WorkspaceFile {
  constructor(readonly uri: string, readonly languageId: string, public version: number, public doc: Text, readonly view: EditorView) {}
  getView(): EditorView { return this.view }
}

class TextFile implements WorkspaceFile {
  readonly version = 0
  constructor(readonly uri: string, readonly languageId: string, readonly doc: Text) {}
  getView(): null { return null }
}

/** Files are open while their editor view holds the LSP plugin; URIs compare by `uriKey`. */
class HostWorkspace extends Workspace {
  files: ViewFile[] = []
  private readonly versions = new Map<string, number>()

  constructor(client: LSPClient, private readonly host: LanguageHost) { super(client) }

  private nextVersion(uri: string): number {
    const key = uriKey(uri)
    const version = (this.versions.get(key) ?? -1) + 1
    this.versions.set(key, version)
    return version
  }

  override getFile(uri: string): ViewFile | null {
    const key = uriKey(uri)
    return this.files.find(file => uriKey(file.uri) === key) ?? null
  }

  syncFiles(): ReturnType<Workspace['syncFiles']> {
    const updates: { changes: ChangeSet; file: WorkspaceFile; prevDoc: Text }[] = []
    for (const file of this.files) {
      const plugin = LSPPlugin.get(file.view)
      if (plugin === null || plugin.unsyncedChanges.empty) continue
      updates.push({ changes: plugin.unsyncedChanges, file, prevDoc: file.doc })
      file.doc = file.view.state.doc
      file.version = this.nextVersion(file.uri)
      plugin.clear()
    }
    return updates
  }

  openFile(uri: string, languageId: string, view: EditorView): void {
    if (this.getFile(uri) !== null) return
    const file = new ViewFile(uri, languageId, this.nextVersion(uri), view.state.doc, view)
    this.files.push(file)
    this.client.didOpen(file)
  }

  closeFile(uri: string, view: EditorView): void {
    const file = this.getFile(uri)
    if (file === null || file.view !== view) return
    this.files = this.files.filter(item => item !== file)
    this.client.didClose(file.uri)
  }

  override async requestFile(uri: string): Promise<WorkspaceFile | null> {
    const open = this.getFile(uri)
    if (open !== null) return open
    const text = await this.host.read(uri)
    return text === undefined ? null : new TextFile(uri, '', Text.of(text.split(/\r?\n/)))
  }

  override displayFile(uri: string): Promise<EditorView | null> {
    return this.host.display(uri)
  }
}

/**
 * Wrap a WebSocket as an LSP transport. Requests the server sends to the client are answered here, because the
 * CodeMirror client answers every server request with MethodNotFound.
 * @param socket Open socket; one JSON-RPC message per text frame.
 * @param answer Result for a server request; throws for methods the editor does not implement.
 * @returns The transport.
 */
function socketTransport(socket: WebSocket, answer: (method: string, params: unknown) => unknown): Transport {
  const handlers = new Set<(value: string) => void>()
  socket.addEventListener('message', (event) => {
    const text = String(event.data)
    let message: { id?: unknown; method?: unknown; params?: unknown }
    try { message = JSON.parse(text) as typeof message } catch (_error) { return } // The Host only relays JSON; anything else is dropped.
    if (message.id !== undefined && typeof message.method === 'string') {
      let reply: object
      try {
        reply = { jsonrpc: '2.0', id: message.id, result: answer(message.method, message.params) ?? null }
      } catch (error) {
        reply = { jsonrpc: '2.0', id: message.id, error: { code: -32601, message: error instanceof Error ? error.message : String(error) } }
      }
      if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify(reply))
      return
    }
    for (const handler of handlers) handler(text)
  })
  return {
    send(message) {
      if (socket.readyState !== WebSocket.OPEN) throw new Error('Language service connection closed.')
      socket.send(message)
    },
    subscribe(handler) { handlers.add(handler) },
    unsubscribe(handler) { handlers.delete(handler) },
  }
}

const ALLOWED_TAGS = new Set(['A', 'B', 'BLOCKQUOTE', 'BR', 'CODE', 'DIV', 'EM', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'HR', 'I', 'LI', 'OL', 'P', 'PRE', 'SPAN', 'STRONG', 'TABLE', 'TBODY', 'TD', 'TH', 'THEAD', 'TR', 'UL'])

/**
 * Strip markup a language server's documentation must not carry. Hover text comes from project source comments,
 * which are untrusted in CTF challenge code, and the page has privileged desktop bridges.
 * @param html Rendered documentation.
 * @returns HTML with only formatting elements, `class` on code spans and `http(s)` links.
 */
export function sanitizeDocumentation(html: string): string {
  const template = document.createElement('template')
  template.innerHTML = html
  const clean = (node: Node): void => {
    for (const child of [...node.childNodes]) {
      if (child.nodeType === Node.TEXT_NODE) continue
      if (!(child instanceof Element) || !ALLOWED_TAGS.has(child.tagName)) {
        if (child instanceof Element && !['SCRIPT', 'STYLE', 'IFRAME', 'OBJECT', 'EMBED', 'TEMPLATE'].includes(child.tagName)) {
          const text = document.createTextNode(child.textContent ?? '')
          child.replaceWith(text)
        } else child.remove()
        continue
      }
      for (const attribute of [...child.attributes]) {
        const keep = (attribute.name === 'class' && (child.tagName === 'CODE' || child.tagName === 'SPAN'))
          || (attribute.name === 'href' && child.tagName === 'A' && /^https?:/i.test(attribute.value))
        if (!keep) child.removeAttribute(attribute.name)
      }
      if (child.tagName === 'A') { child.setAttribute('target', '_blank'); child.setAttribute('rel', 'noreferrer') }
      clean(child)
    }
  }
  clean(template.content)
  return template.innerHTML
}

/** Editor features every language-server document gets. */
const clientExtensions = [
  serverCompletion(),
  hoverTooltips(),
  signatureHelp(),
  serverDiagnostics(),
  keymap.of([...jumpToDefinitionKeymap, ...findReferencesKeymap]),
]

/** One language server for one project root. */
export class LanguageConnection {
  readonly client: LSPClient
  /** Settles when the server finished `initialize`; rejects when the connection fails first. */
  readonly ready: Promise<void>
  private readonly socket: WebSocket
  private closed = false

  /**
   * Open the connection.
   * @param target Project root and server language.
   * @param host Editor services.
   * @param onClose Called once when the socket closes without `close()`, with the reason.
   */
  constructor(readonly target: LanguageTarget, host: LanguageHost, onClose: (message: string) => void) {
    const url = new URL('/rainy/ide/lsp', location.href)
    url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:'
    url.searchParams.set('workspaceId', target.workspaceId)
    url.searchParams.set('language', target.language)
    url.searchParams.set('rootId', target.rootId)
    const rootUri = directoryUri(target.root)
    const python = target.language === 'python' && target.pythonPath !== undefined
    this.client = new LSPClient({
      rootUri,
      timeout: REQUEST_TIMEOUT_MS,
      sanitizeHTML: sanitizeDocumentation,
      ...(python ? { initializationOptions: { pythonPath: target.pythonPath } } : {}),
      workspace: client => new HostWorkspace(client, host),
      extensions: clientExtensions,
      notificationHandlers: {
        'textDocument/publishDiagnostics': (_client, params: { uri: string; diagnostics: LspDiagnostic[] }) => {
          host.diagnostics(params.uri, params.diagnostics)
          return false
        },
      },
    })
    this.socket = new WebSocket(url)
    const answer = (method: string, params: unknown): unknown => {
      switch (method) {
        case 'workspace/configuration': {
          const items = (params as { items?: { section?: string }[] } | undefined)?.items ?? []
          return items.map((item) => {
            if (!python) return null
            if (item.section === 'python') return { pythonPath: target.pythonPath }
            if (item.section === 'python.pythonPath') return target.pythonPath
            return null
          })
        }
        case 'workspace/workspaceFolders': return [{ uri: rootUri, name: target.title }]
        case 'client/registerCapability':
        case 'client/unregisterCapability':
        case 'window/workDoneProgress/create':
        case 'window/showMessageRequest': return null
        case 'workspace/applyEdit': return { applied: false }
        default: throw new Error(`Method not implemented: ${method}`)
      }
    }
    this.ready = new Promise<void>((resolve, reject) => {
      this.socket.addEventListener('open', () => {
        this.client.connect(socketTransport(this.socket, answer))
        this.client.initializing.then(() => {
          if (python) this.client.notification('workspace/didChangeConfiguration', { settings: { python: { pythonPath: target.pythonPath } } })
          resolve()
        }, (error: unknown) => { reject(new Error(typeof error === 'object' && error !== null && 'message' in error ? String(error.message) : String(error))) })
      }, { once: true })
      this.socket.addEventListener('close', (event) => {
        const message = event.reason !== '' ? event.reason : 'Language service connection closed.'
        reject(new Error(message))
        if (!this.closed) { this.closed = true; this.client.disconnect(); onClose(message) }
      }, { once: true })
    })
    // Rejections are reported through `onClose` or the caller awaiting `ready`.
    this.ready.catch(() => undefined)
  }

  /**
   * Editor extension that opens a document with this server.
   * @param uri Document URI.
   * @param languageId LSP language id.
   * @returns The extension.
   */
  plugin(uri: string, languageId: string): Extension {
    return this.client.plugin(uri, languageId)
  }

  /** Disconnect and close the socket; the Host stops the server. */
  close(): void {
    if (this.closed) return
    this.closed = true
    this.client.disconnect()
    this.socket.close(1000)
  }
}
