/** Workspace-owned source buffers and durable recovery, independent of conversation mounts. */
import { createSnapshotStore } from '@deepseek-ai/dsh-client-store'
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import type {
  IdeDirectory,
  IdeEditorTab,
  IdeFileDocument,
  IdeFileEntry,
  IdeFileVersion,
  IdeLayoutState,
  IdeWorkspace,
  IdeWorkspaceState,
  IdeWorkspaceStateData,
  WorkspaceId,
  IdeRootId,
} from '../ide-files-protocol.ts'
import type { IdeExecutionConfiguration } from '../ide-execution-protocol.ts'
import type { EditorDocument, EditorProblem, EditorSelection, EditorView } from './editor-types.ts'
import { IdeRequestError, type IdeFilesApi } from './ide-api.ts'
import { createRootedIdeApi } from './ide-rooted-api.ts'
import { absoluteFilePath, fileKey, fileReference, keyFromAbsolute, workspaceRoots } from './ide-paths.ts'

/** A complete buffer with its last observed disk contents. */
export interface IdeBuffer {
  readonly document: Omit<IdeFileDocument, 'workspaceId' | 'version'> & {
    readonly workspaceId: WorkspaceId | null
    readonly version: IdeFileVersion | null
  }
  readonly text: string
  readonly dirty: boolean
  readonly external: boolean
  readonly source?: 'snippet' | undefined
  readonly language?: string | undefined
  readonly comparison?: {
    readonly original: string
    readonly kind: 'git' | 'conflict' | 'snippet'
    readonly version?: IdeFileVersion
    readonly target?: string | undefined
  }
}

/** Snapshot consumed by the framework-generated workspace hook. */
export interface IdeState {
  readonly workspaces: readonly IdeWorkspace[]
  readonly workspace: IdeWorkspace | null
  readonly phase: 'loading' | 'ready' | 'error'
  readonly error: string
  readonly saving: boolean
  readonly recoveryConflict: boolean
  readonly data: IdeWorkspaceStateData
  readonly buffers: Readonly<Record<string, IdeBuffer>>
  readonly directories: Readonly<Record<string, IdeDirectory>>
  readonly problems: readonly EditorProblem[]
  readonly selection: EditorSelection | undefined
  readonly languageStates: Readonly<Record<string, string>>
  readonly center: 'editor' | 'tools'
  readonly quickOpen: boolean
  readonly reveal?: { readonly path: string; readonly line: number; readonly column: number }
}

/** The owner supplies session restoration and UI feedback without sharing feature implementations. */
export interface IdeModelOptions {
  readonly debounceMs: number
  readonly pollMs: number
  readonly restoreSession: (workspace: IdeWorkspace, sessionId: SessionId | null) => Promise<void>
  readonly isSessionSelected?: ((sessionId: SessionId) => boolean) | undefined
  readonly describeError?: ((error: unknown) => string) | undefined
}

const initialData = (): IdeWorkspaceStateData => ({
  lastSessionId: null,
  tabs: [],
  activePath: null,
  expandedPaths: [],
  buffers: [],
  layout: {
    sidebarWidth: 240,
    agentWidth: 400,
    bottomHeight: 240,
    sidebarVisible: true,
    agentVisible: false,
    bottomVisible: false,
    bottomTab: 'terminal',
  },
  execution: { profiles: [], activeProfile: null, breakpoints: [], watches: [] },
})

/** Infer the editor language from a source filename. @param path Source path. @returns Monaco language identity. */
export function sourceLanguage(path: string): string {
  const extension = path.split('.').at(-1)?.toLowerCase()
  return (
    (
      {
        py: 'python',
        php: 'php',
        pyi: 'python',
        js: 'javascript',
        mjs: 'javascript',
        cjs: 'javascript',
        jsx: 'javascript',
        ts: 'typescript',
        tsx: 'typescript',
        c: 'c',
        h: 'c',
        cc: 'cpp',
        cpp: 'cpp',
        cxx: 'cpp',
        hpp: 'cpp',
        json: 'json',
        md: 'markdown',
        yml: 'yaml',
        yaml: 'yaml',
        html: 'html',
        css: 'css',
        sh: 'shell',
        txt: 'plaintext',
      } as Record<string, string>
    )[extension ?? ''] ?? 'plaintext'
  )
}

/**
 * Resolve a source URI; removed mounts retain an isolated recovery model without a language connection.
 * @param workspace Host root.
 * @param path Root-qualified editor key.
 * @returns File URI, or an untitled recovery URI for a removed mount.
 */
export function sourceUri(workspace: IdeWorkspace, path: string): string {
  const absolute = absoluteFilePath(workspace, path)
  if (absolute === undefined) return `untitled:rainy-recovery/${encodeURIComponent(workspace.workspaceId)}/${encodeURIComponent(path)}`
  const windowsDrive = /^[A-Za-z]:\//u.test(absolute)
  const prefix = windowsDrive ? 'file:///' : absolute.startsWith('//') ? 'file:' : 'file://'
  return prefix + absolute.split('/').map((part, index) => windowsDrive && index === 0 ? part : encodeURIComponent(part)).join('/')
}

/** Retains dirty files across panel changes and saves recovery through revision comparisons. */
export class IdeModel {
  readonly state = createSnapshotStore<IdeState>({
    workspaces: [],
    workspace: null,
    phase: 'loading',
    error: '',
    saving: false,
    recoveryConflict: false,
    data: initialData(),
    buffers: {},
    directories: {},
    problems: [],
    selection: undefined,
    languageStates: {},
    center: 'editor',
    quickOpen: false,
  })
  private revision = 0
  private generation = 0
  private persistedGeneration = 0
  private disposed = false
  private switching = false
  private timer: ReturnType<typeof setTimeout> | undefined
  private polling: ReturnType<typeof setTimeout> | undefined
  private persistence: Promise<boolean> | undefined
  private snippetCounter = 0
  private readonly api: IdeFilesApi

  /** @param api Validated Host file adapter. @param options Timing and session callbacks. */
  constructor(
    api: IdeFilesApi,
    private readonly options: IdeModelOptions,
  ) { this.api = createRootedIdeApi(api) }

  /**
   * Load registered projects and restore the Host's saved selection across browser origins.
   * @returns Completed initialization.
   */
  async initialize(): Promise<void> {
    try {
      const [workspaces, selection] = await Promise.all([
        this.api.request({ op: 'workspaces.list' }),
        this.api.request({ op: 'state.selection.read' }),
      ])
      if (this.disposed) return
      this.patch({ workspaces, phase: 'ready' })
      const selected = workspaces.find(workspace => workspace.workspaceId === selection.workspaceId)
      if (selected !== undefined) await this.selectWorkspace(selected)
    } catch (error) {
      this.fail(error)
    }
    this.schedulePoll()
  }

  /** Register a directory without starting a chat.
   * @param path Absolute Host directory.
   * @param sessionId Existing conversation explicitly selected from history.
   * @returns Completed selection.
   */
  async openWorkspace(path: string, sessionId?: SessionId): Promise<void> {
    const workspace = await this.api.request({ op: 'workspaces.open', path })
    const workspaces = this.state
      .getSnapshot()
      .workspaces.filter(entry => entry.workspaceId !== workspace.workspaceId)
    this.patch({ workspaces: [...workspaces, workspace] })
    await this.selectWorkspace(workspace, sessionId)
  }

  /** Attach an existing directory without changing the project's chat or primary cwd.
   * @param path Absolute Host directory chosen by the user.
   * @returns Completion after the new root becomes visible.
   */
  async attachRoot(path: string): Promise<void> {
    const current = this.state.getSnapshot()
    if (current.workspace === null) { await this.openWorkspace(path); return }
    if (!(await this.flush())) return
    const workspace = await this.api.request({ op: 'workspaces.attach', workspaceId: current.workspace.workspaceId, path })
    this.patch({ workspaces: this.state.getSnapshot().workspaces.map(item =>
      item.workspaceId === workspace.workspaceId ? workspace : item) })
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return
    this.patch({ workspace, error: '' })
    await this.refreshTree()
  }

  /** Remove a mounted root after its dirty files have been saved or explicitly discarded.
   * @param rootId Secondary directory identity.
   * @returns Completion without deleting any source files.
   */
  async removeRoot(rootId: IdeRootId): Promise<void> {
    const current = this.state.getSnapshot()
    if (current.workspace === null) return
    if (Object.entries(current.buffers).some(([path, buffer]) => fileReference(path).rootId === rootId && buffer.dirty))
      throw new IdeRequestError('unsaved-root', 'Save or close the modified files before removing this folder.')
    if (!(await this.flush())) return
    const workspace = await this.api.request({ op: 'workspaces.removeRoot', workspaceId: current.workspace.workspaceId, rootId })
    this.patch({ workspaces: this.state.getSnapshot().workspaces.map(item =>
      item.workspaceId === workspace.workspaceId ? workspace : item) })
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return
    for (const tab of this.state.getSnapshot().data.tabs) if (fileReference(tab.path).rootId === rootId) this.close(tab.path)
    this.patch({ workspace, directories: Object.fromEntries(Object.entries(this.state.getSnapshot().directories)
      .filter(([path]) => fileReference(path).rootId !== rootId)),
    error: '' })
    this.setData({ expandedPaths: this.state.getSnapshot().data.expandedPaths.filter(path => fileReference(path).rootId !== rootId) })
    await this.flush()
  }

  /**
   * Flush recovery before changing the workspace; a failed flush preserves the current project.
   * The restored file tree becomes interactive after session restoration and selection persistence complete.
   * @param workspace Selected project.
   * @param sessionId Existing conversation selected from history; omitted restores the saved selection.
   * @returns Completed restore.
   */
  async selectWorkspace(workspace: IdeWorkspace, sessionId?: SessionId): Promise<void> {
    if (this.switching || this.disposed || this.state.getSnapshot().workspace?.workspaceId === workspace.workspaceId)
      return
    this.switching = true
    const previous = this.state.getSnapshot()
    let restoredSession = false
    this.patch({ phase: 'loading', error: '' })
    try {
      if (!(await this.flush())) return
      const saved = await this.api.request({ op: 'state.read', workspaceId: workspace.workspaceId })
      const buffers: Record<string, IdeBuffer> = {}
      const tabs: IdeEditorTab[] = []
      for (const tab of saved.data.tabs) {
        const recovery = saved.data.buffers.find(buffer => buffer.path === tab.path)
        try {
          const document = await this.api.request({
            op: 'files.read',
            workspaceId: workspace.workspaceId,
            path: tab.path,
          })
          const dirty = recovery !== undefined && recovery.content !== document.content
          const buffer: IdeBuffer = {
            document,
            text: recovery?.content ?? document.content ?? '',
            dirty,
            external: dirty && recovery.baseVersion !== document.version,
          }
          buffers[tab.path] =
            dirty && recovery.baseVersion !== null
              ? { ...buffer, document: { ...document, version: recovery.baseVersion } }
              : buffer
          tabs.push({ ...tab, kind: 'file' })
        } catch (error) {
          if (recovery === undefined) continue
          if (!(error instanceof IdeRequestError) || !['not-found', 'workspace-unavailable', 'workspace-not-found'].includes(error.code) || recovery.baseVersion === null)
            throw error
          buffers[tab.path] = {
            document: {
              workspaceId: workspace.workspaceId,
              path: tab.path,
              version: recovery.baseVersion,
              bytes: new TextEncoder().encode(recovery.content).length,
              content: '',
              bom: recovery.bom,
              eol: recovery.eol,
              readOnlyReason: null,
            },
            text: recovery.content,
            dirty: true,
            external: true,
          }
          tabs.push({ ...tab, kind: 'file' })
        }
      }
      const directories: Record<string, IdeDirectory> = {}
      const expandedPaths: string[] = []
      for (const root of workspaceRoots(workspace)) {
        const key = fileKey('', root.rootId)
        try { directories[key] = await this.api.request({ op: 'files.list', workspaceId: workspace.workspaceId, path: key }) }
        catch (error) {
          if (root.primary || !(error instanceof IdeRequestError)
            || !['workspace-not-found', 'workspace-unavailable', 'not-found'].includes(error.code)) throw error
        }
      }
      for (const path of saved.data.expandedPaths) {
        try {
          directories[path] = await this.api.request({ op: 'files.list', workspaceId: workspace.workspaceId, path })
          expandedPaths.push(path)
        } catch (error) {
          if (!(error instanceof IdeRequestError) || !['not-found', 'not-directory', 'workspace-not-found', 'workspace-unavailable'].includes(error.code)) throw error
        }
      }
      if (this.isDisposed()) return
      const data = {
        ...saved.data,
        // A conversation opened from history stays visible whatever pane visibility the project saved.
        ...sessionId === undefined ? {} : { layout: { ...saved.data.layout, agentVisible: true } },
        lastSessionId: sessionId ?? saved.data.lastSessionId,
        tabs,
        expandedPaths,
        activePath: tabs.some(tab => tab.path === saved.data.activePath)
          ? saved.data.activePath
          : (tabs[0]?.path ?? null),
      }
      if (sessionId !== undefined && this.options.isSessionSelected?.(sessionId) === false) return
      await this.options.restoreSession(workspace, data.lastSessionId)
      restoredSession = true
      await this.api.request({ op: 'state.selection.save', workspaceId: workspace.workspaceId })
      if (this.isDisposed()) return
      this.revision = saved.revision
      this.generation = 0
      this.persistedGeneration = 0
      this.patch({
        workspace,
        data,
        buffers,
        directories,
        phase: 'ready',
        saving: false,
        recoveryConflict: false,
        selection: undefined,
        problems: [],
        languageStates: {},
        center: 'editor',
      })
      if (saved.data.buffers.length !== Object.values(buffers).filter(buffer => buffer.dirty).length
        || saved.data.expandedPaths.length !== expandedPaths.length || saved.data.tabs.length !== tabs.length
        || data.lastSessionId !== saved.data.lastSessionId || data.layout.agentVisible !== saved.data.layout.agentVisible) this.changed()
    } catch (error) {
      if (restoredSession)
        await this.options.restoreSession(previous.workspace ?? workspace, previous.workspace === null ? null : previous.data.lastSessionId)
          .catch((restoreError: unknown) => { this.fail(restoreError) })
      this.fail(error)
    } finally {
      this.switching = false
      if (!this.isDisposed() && this.state.getSnapshot().phase === 'loading') {
        const current = this.state.getSnapshot()
        this.patch({ phase: current.workspace === null && current.error !== '' ? 'error' : 'ready' })
      }
    }
  }

  /** Read one directory. @param path Project-relative directory. @returns Complete listing. */
  async loadDirectory(path: string): Promise<void> {
    const workspace = this.state.getSnapshot().workspace
    if (workspace === null) return
    const directory = await this.api.request({ op: 'files.list', workspaceId: workspace.workspaceId, path })
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return
    this.patch({ directories: { ...this.state.getSnapshot().directories, [path]: directory } })
  }

  /**
   * Search contained workspace filenames.
   * @param query Filename or relative path fragment.
   * @param signal Superseded query cancellation.
   * @returns Matching paths.
   */
  async searchFiles(
    query: string,
    signal?: AbortSignal,
  ): Promise<{ readonly paths: readonly string[]; readonly truncated: boolean }> {
    const workspace = this.state.getSnapshot().workspace
    if (workspace === null) return { paths: [], truncated: false }
    const results = await Promise.all(workspaceRoots(workspace).map(async (root) => {
      try {
        const result = await this.api.request({ op: 'files.search', workspaceId: workspace.workspaceId,
          ...(root.primary ? {} : { rootId: root.rootId }), query }, signal)
        return { ...result, paths: result.paths.map(path => fileKey(path, root.rootId)) }
      } catch (error) {
        if (root.primary || !(error instanceof IdeRequestError) || !['workspace-unavailable', 'workspace-not-found'].includes(error.code)) throw error
        return { paths: [], truncated: true }
      }
    }))
    return { paths: results.flatMap(result => result.paths), truncated: results.some(result => result.truncated) }
  }

  /** Toggle the quick-open dialog. @param open Whether to show filename search. */
  quickOpen(open: boolean): void {
    this.patch({ quickOpen: open })
  }

  /** Expand or collapse a folder while retaining its children. @param path Project-relative directory. @returns Listing completion. */
  async toggleDirectory(path: string): Promise<void> {
    const expanded = this.state.getSnapshot().data.expandedPaths
    const next = expanded.includes(path) ? expanded.filter(entry => entry !== path) : [...expanded, path]
    if (next.includes(path)) await this.loadDirectory(path)
    this.setData({ expandedPaths: next })
  }

  /** Open complete source contents in a retained tab.
   * @param path Project-relative source.
   * @param activate Whether to select the tab and reveal the editor.
   * @returns Open buffer.
   */
  async openFile(path: string, activate = true): Promise<IdeBuffer | undefined> {
    if (this.switching) return undefined
    const before = this.state.getSnapshot()
    if (before.workspace === null) return undefined
    if (before.buffers[path] !== undefined) {
      if (activate) {
        this.setData({ activePath: path })
        this.patch({ center: 'editor' })
      }
      return before.buffers[path]
    }
    const document = await this.api.request({ op: 'files.read', workspaceId: before.workspace.workspaceId, path })
    if (this.state.getSnapshot().workspace?.workspaceId !== before.workspace.workspaceId) return undefined
    const current = this.state.getSnapshot()
    if (current.buffers[path] !== undefined) {
      if (activate) {
        this.setData({ activePath: path })
        this.patch({ center: 'editor' })
      }
      return current.buffers[path]
    }
    const buffer: IdeBuffer = { document, text: document.content ?? '', dirty: false, external: false }
    this.patch({ buffers: { ...current.buffers, [path]: buffer }, ...(activate ? { center: 'editor' } : {}) })
    this.setData({ tabs: [...current.data.tabs, { path, kind: 'file' }], ...(activate ? { activePath: path } : {}) })
    return buffer
  }

  /** Open a readonly AI code preview without altering a source buffer.
   * @param code Assistant fence contents.
   * @param language Fence language hint.
   * @param title Localized preview name.
   * @param compare Whether to compare against the selected editable source.
   */
  openSnippet(code: string, language: string, title: string, compare: boolean): void {
    const current = this.state.getSnapshot()
    const target = current.data.activePath
    const original = target === null ? undefined : current.buffers[target]
    if (
      compare &&
      (target === null ||
        original === undefined ||
        original.source === 'snippet' ||
        original.document.readOnlyReason !== null)
    )
      return
    const normalized =
      ({ py: 'python', js: 'javascript', ts: 'typescript', 'c++': 'cpp' } as Record<string, string>)[language] ??
      language
    const extension =
      (
        {
          python: 'py',
          javascript: 'js',
          typescript: 'ts',
          c: 'c',
          cpp: 'cpp',
          json: 'json',
          html: 'html',
          css: 'css',
        } as Record<string, string>
      )[normalized] ?? 'txt'
    const number = ++this.snippetCounter
    const path = `rainy-snippet:${number}/${title} ${number}.${extension}`
    const buffer: IdeBuffer = {
      source: 'snippet',
      language: normalized || 'plaintext',
      text: code,
      dirty: false,
      external: false,
      document: {
        workspaceId: null,
        path,
        version: null,
        content: code,
        bytes: new TextEncoder().encode(code).length,
        bom: false,
        eol: 'lf',
        readOnlyReason: null,
      },
      ...(compare && original !== undefined && target !== null
        ? { comparison: { original: original.text, kind: 'snippet' as const, target } }
        : {}),
    }
    this.patch({ buffers: { ...current.buffers, [path]: buffer }, center: 'editor' })
    this.setData({ tabs: [...current.data.tabs, { path, kind: compare ? 'diff' : 'file' }], activePath: path })
  }

  /** Apply a reviewed AI comparison to the source buffer without saving its file.
   * @param path Readonly snippet tab.
   * @returns Applied, a refreshed comparison after source changes, or a missing target.
   */
  async applySnippet(path: string): Promise<'applied' | 'changed' | 'missing'> {
    const current = this.state.getSnapshot()
    const snippet = current.buffers[path]
    const comparison = snippet?.comparison
    if (snippet?.source !== 'snippet' || comparison?.kind !== 'snippet' || comparison.target === undefined)
      return 'missing'
    const target = current.buffers[comparison.target]
    if (target === undefined || target.source === 'snippet' || target.document.readOnlyReason !== null) return 'missing'
    if (target.text !== comparison.original) {
      this.patch({
        buffers: { ...current.buffers, [path]: { ...snippet, comparison: { ...comparison, original: target.text } } },
      })
      return 'changed'
    }
    this.change(comparison.target, snippet.text)
    this.patch({ error: '' })
    await this.openFile(comparison.target)
    return 'applied'
  }

  /**
   * Resolve an LSP URI inside the active workspace, retaining edited files for recovery and explicit saves.
   * @param uri File URI.
   * @param mode Read without retaining, open and select, or retain for a language edit without selecting.
   * @returns Complete editor document.
   */
  async openUri(uri: string, mode: 'read' | 'open' | 'edit'): Promise<EditorDocument | undefined> {
    const workspace = this.state.getSnapshot().workspace
    if (workspace === null) return undefined
    const parsed = new URL(uri)
    const path = decodeURIComponent(parsed.pathname)
    if (parsed.protocol !== 'file:') return undefined
    const absolute = parsed.host !== '' ? `//${parsed.host}${path}` : /^\/[A-Za-z]:\//u.test(path) ? path.slice(1) : path
    const relative = keyFromAbsolute(workspace, absolute)
    if (relative === undefined) return undefined
    if (mode !== 'read') await this.openFile(relative, mode === 'open')
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return undefined
    const current = this.state.getSnapshot().buffers[relative]
    const document =
      current?.document ??
      (await this.api.request({ op: 'files.read', workspaceId: workspace.workspaceId, path: relative }))
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return undefined
    if (document.content === null) return undefined
    return {
      path: relative,
      uri,
      language: sourceLanguage(relative),
      text: current?.text ?? document.content,
      readOnly: document.readOnlyReason !== null,
    }
  }

  /** Update source text without writing its file. @param path Open source. @param text Complete buffer. */
  change(path: string, text: string): void {
    if (this.switching) return
    const current = this.state.getSnapshot()
    const buffer = current.buffers[path]
    if (
      buffer === undefined ||
      buffer.source === 'snippet' ||
      buffer.document.readOnlyReason !== null ||
      buffer.text === text
    )
      return
    this.patch({
      buffers: { ...current.buffers, [path]: { ...buffer, text, dirty: text !== buffer.document.content } },
    })
    this.changed()
  }

  /** Persist a cursor and scroll position. @param path Open source. @param view Monaco position. */
  view(path: string, view: EditorView): void {
    const tabs = this.state
      .getSnapshot()
      .data.tabs.map(tab =>
        tab.path === path
          ? { ...tab, cursor: { line: view.line, column: view.column }, scroll: { top: view.top, left: view.left } }
          : tab,
      )
    this.setData({ tabs })
  }

  /**
   * Save using the observed version; conflicts expose a diff and retain the local buffer.
   * @param path Source, defaulting to selected.
   * @returns Whether disk save completed.
   */
  async save(path = this.state.getSnapshot().data.activePath): Promise<boolean> {
    const before = this.state.getSnapshot()
    const buffer = path === null ? undefined : before.buffers[path]
    if (
      before.workspace === null ||
      buffer === undefined ||
      buffer.source === 'snippet' ||
      buffer.document.version === null ||
      path === null ||
      buffer.document.readOnlyReason !== null
    )
      return false
    if (!buffer.dirty) return true
    try {
      const document = await this.api.request({
        op: 'files.save',
        workspaceId: before.workspace.workspaceId,
        path,
        content: buffer.text,
        expectedVersion: buffer.document.version,
      })
      const current = this.state.getSnapshot()
      if (current.workspace?.workspaceId !== before.workspace.workspaceId) return true
      const latest = current.buffers[path]
      if (latest === undefined) return true
      this.patch({
        buffers: {
          ...current.buffers,
          [path]: { document, text: latest.text, dirty: latest.text !== document.content, external: false },
        },
        error: '',
      })
      this.changed()
      return true
    } catch (error) {
      if (error instanceof IdeRequestError && error.code === 'version-conflict') {
        let disk: IdeFileDocument | undefined
        // A file deleted on disk cannot be compared; the conflict itself is still reported.
        try { disk = await this.api.request({ op: 'files.read', workspaceId: before.workspace.workspaceId, path }) }
        catch (_unreadableDisk) { disk = undefined }
        const current = this.state.getSnapshot()
        if (current.workspace?.workspaceId !== before.workspace.workspaceId) return false
        const latest = current.buffers[path]
        if (latest !== undefined && disk !== undefined) {
          this.patch({
            buffers: {
              ...current.buffers,
              [path]: {
                ...latest,
                external: true,
                comparison: { original: disk.content ?? '', kind: 'conflict', version: disk.version },
              },
            },
          })
          this.setData({
            activePath: path,
            tabs: current.data.tabs.map(tab => (tab.path === path ? { ...tab, kind: 'diff' } : tab)),
          })
        }
      }
      this.fail(error)
      return false
    }
  }

  /**
   * Save every dirty source before a human launch; edits arriving during the save prevent launch.
   * @returns Whether all buffers are clean.
   */
  async saveAll(): Promise<boolean> {
    for (const [path, buffer] of Object.entries(this.state.getSnapshot().buffers)) {
      if (buffer.dirty && !(await this.save(path))) return false
    }
    return Object.values(this.state.getSnapshot().buffers).every(buffer => !buffer.dirty)
  }

  /**
   * Accept the observed disk version for an explicitly reviewed conflict; the next save still compares it.
   * @param path Conflicting source.
   * @returns Completed read.
   */
  acceptConflict(path: string): void {
    const current = this.state.getSnapshot()
    const buffer = current.buffers[path]
    const comparison = buffer?.comparison
    if (buffer === undefined || comparison?.kind !== 'conflict' || comparison.version === undefined) return
    this.patch({
      buffers: {
        ...current.buffers,
        [path]: {
          ...buffer,
          document: { ...buffer.document, version: comparison.version, content: comparison.original },
          dirty: buffer.text !== comparison.original,
          external: false,
        },
      },
      error: '',
    })
    this.changed()
  }

  /** Reload a file after the caller has confirmed discarding changes. @param path Open source. @returns Completed reload. */
  async reload(path: string): Promise<void> {
    const before = this.state.getSnapshot()
    if (before.workspace === null) return
    const document = await this.api.request({ op: 'files.read', workspaceId: before.workspace.workspaceId, path })
    const current = this.state.getSnapshot()
    if (current.workspace?.workspaceId !== before.workspace.workspaceId) return
    this.patch({
      buffers: {
        ...current.buffers,
        [path]: { document, text: document.content ?? '', dirty: false, external: false },
      },
      error: '',
    })
    this.setData({ tabs: current.data.tabs.map(tab => (tab.path === path ? { ...tab, kind: 'file' } : tab)) })
  }

  /** Close one tab after UI confirmation of any dirty buffer. @param path Open source. */
  close(path: string): void {
    const current = this.state.getSnapshot()
    const buffers = Object.fromEntries(Object.entries(current.buffers).filter(([entry]) => entry !== path))
    const tabs = current.data.tabs.filter(tab => tab.path !== path)
    this.patch({ buffers })
    this.setData({
      tabs,
      activePath: current.data.activePath === path ? (tabs.at(-1)?.path ?? null) : current.data.activePath,
    })
  }

  /** Open the Git comparison without changing disk. @param path Source. @returns Completed comparison. */
  async diff(path: string): Promise<void> {
    const workspace = this.state.getSnapshot().workspace
    if (workspace === null) return
    await this.openFile(path)
    const comparison = await this.api.request({ op: 'files.diff', workspaceId: workspace.workspaceId, path })
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return
    if (comparison.base === null || comparison.current === null)
      throw new IdeRequestError(comparison.reason ?? 'diff-unavailable', 'File comparison is unavailable.')
    const current = this.state.getSnapshot()
    const buffer = current.buffers[path]
    if (buffer === undefined) return
    this.patch({
      buffers: { ...current.buffers, [path]: { ...buffer, comparison: { original: comparison.base, kind: 'git' } } },
    })
    this.setData({
      activePath: path,
      tabs: current.data.tabs.map(tab => (tab.path === path ? { ...tab, kind: 'diff' } : tab)),
    })
  }

  /** Return from a diff to the editable file. @param path Source. */
  edit(path: string): void {
    this.setData({
      tabs: this.state.getSnapshot().data.tabs.map(tab => (tab.path === path ? { ...tab, kind: 'file' } : tab)),
    })
  }

  /** Format the selected buffer using the prepared workspace formatter. @returns Completed buffer update. */
  async format(): Promise<void> {
    const current = this.state.getSnapshot()
    const path = current.data.activePath
    if (path === null || current.workspace === null) return
    const buffer = current.buffers[path]
    if (buffer === undefined || buffer.source === 'snippet' || buffer.document.readOnlyReason !== null) return
    const formatted = await this.api.request({
      op: 'format',
      workspaceId: current.workspace.workspaceId,
      path,
      text: buffer.text,
      language: sourceLanguage(path),
    })
    if (
      this.state.getSnapshot().workspace?.workspaceId === current.workspace.workspaceId &&
      this.state.getSnapshot().buffers[path]?.text === buffer.text
    )
      this.change(path, formatted.text)
  }

  /**
   * Create a child path and refresh its parent.
   * @param path New relative path.
   * @param directory Whether to create a directory.
   * @returns Completed creation.
   */
  async create(path: string, directory: boolean): Promise<void> {
    const workspace = this.state.getSnapshot().workspace
    if (workspace === null) return
    if (directory) await this.api.request({ op: 'files.mkdir', workspaceId: workspace.workspaceId, path })
    else await this.api.request({ op: 'files.create', workspaceId: workspace.workspaceId, path, content: '' })
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return
    const reference = fileReference(path)
    const parent = reference.path.includes('/') ? reference.path.slice(0, reference.path.lastIndexOf('/')) : ''
    await this.loadDirectory(fileKey(parent, reference.rootId))
    if (!directory) await this.openFile(path)
  }

  /**
   * Rename an observed tree item, retaining any open local text under its new path.
   * @param entry Observed item.
   * @param destination New relative path.
   * @returns Completed move.
   */
  async rename(entry: IdeFileEntry, destination: string): Promise<void> {
    const before = this.state.getSnapshot()
    if (before.workspace === null) return
    await this.api.request({
      op: 'files.rename',
      workspaceId: before.workspace.workspaceId,
      path: entry.path,
      destination,
      expectedVersion: entry.version,
    })
    if (this.state.getSnapshot().workspace?.workspaceId !== before.workspace.workspaceId) return
    const moved = (path: string): string =>
      path === entry.path
        ? destination
        : path.startsWith(entry.path + '/')
          ? destination + path.slice(entry.path.length)
          : path
    const buffers: Record<string, IdeBuffer> = {}
    for (const [path, buffer] of Object.entries(this.state.getSnapshot().buffers)) {
      const next = moved(path)
      const document =
        next === path
          ? buffer.document
          : await this.api.request({ op: 'files.read', workspaceId: before.workspace.workspaceId, path: next })
      const external = buffer.dirty && document.content !== buffer.document.content
      buffers[next] = {
        ...buffer,
        document: external
          ? { ...document, version: buffer.document.version, content: buffer.document.content }
          : document,
        external: buffer.external || external,
      }
    }
    const current = this.state.getSnapshot()
    if (current.workspace?.workspaceId !== before.workspace.workspaceId) return
    this.patch({ buffers, directories: {} })
    this.setData({
      tabs: current.data.tabs.map(tab => ({ ...tab, path: moved(tab.path) })),
      activePath: current.data.activePath === null ? null : moved(current.data.activePath),
      expandedPaths: current.data.expandedPaths.map(moved),
    })
    await this.refreshTree()
  }

  /**
   * Remove only the subtree that the caller explicitly confirms.
   * @param path Selected path.
   * @param confirm Exact preview confirmation callback.
   * @returns Completed deletion.
   */
  async remove(path: string, confirm: (entries: number, bytes: number) => Promise<boolean>): Promise<void> {
    const workspace = this.state.getSnapshot().workspace
    if (workspace === null) return
    const preview = await this.api.request({ op: 'files.deletePreview', workspaceId: workspace.workspaceId, path })
    if (!(await confirm(preview.entries, preview.bytes))) return
    await this.api.request({ op: 'files.delete', workspaceId: workspace.workspaceId, path, token: preview.token })
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return
    for (const tab of this.state.getSnapshot().data.tabs)
      if (tab.path === path || tab.path.startsWith(path + '/')) this.close(tab.path)
    this.setData({
      expandedPaths: this.state
        .getSnapshot()
        .data.expandedPaths.filter(entry => entry !== path && !entry.startsWith(path + '/')),
    })
    await this.refreshTree()
  }

  /** Refresh all expanded tree directories. @returns Completed listings. */
  async refreshTree(): Promise<void> {
    const current = this.state.getSnapshot()
    if (current.workspace === null) return
    const missing = new Set<string>()
    for (const path of [...workspaceRoots(current.workspace).map(root => fileKey('', root.rootId)), ...current.data.expandedPaths]) {
      try { await this.loadDirectory(path) }
      catch (error) {
        if (path !== '' && error instanceof IdeRequestError && ['not-found', 'not-directory', 'workspace-not-found', 'workspace-unavailable'].includes(error.code)) missing.add(path)
        else throw error
      }
    }
    if (missing.size > 0) {
      this.patch({ directories: Object.fromEntries(Object.entries(this.state.getSnapshot().directories)
        .filter(([path]) => !missing.has(path))) })
      this.setData({ expandedPaths: this.state.getSnapshot().data.expandedPaths.filter(path => !missing.has(path)) })
    }
  }
  /** Remember the selected chat without changing its messages. @param sessionId Existing or newly selected chat. */
  session(sessionId: SessionId | null): void {
    const current = this.state.getSnapshot()
    if (!this.switching && current.workspace !== null && current.data.lastSessionId !== sessionId)
      this.setData({ lastSessionId: sessionId })
  }
  /** Update workspace geometry. @param layout Changed values. */
  layout(layout: Partial<IdeLayoutState>): void {
    this.setData({ layout: { ...this.state.getSnapshot().data.layout, ...layout } })
  }
  /** Update saved run/debug choices. @param execution Human configuration. */
  execution(execution: IdeExecutionConfiguration): void {
    this.setData({ execution })
  }
  /** Select a retained central surface. @param center Editor or tool workbench. */
  center(center: IdeState['center']): void {
    this.patch({ center })
  }
  /**
   * Open and reveal a contained source position.
   * @param path Relative or contained absolute source.
   * @param line One-based line.
   * @param column One-based column.
   * @returns Completed navigation.
   */
  async reveal(path: string, line: number, column: number): Promise<void> {
    const workspace = this.state.getSnapshot().workspace
    if (workspace === null) return
    const absolute = path.startsWith('/') || /^[A-Za-z]:[\\/]/u.test(path)
    const relative = absolute ? keyFromAbsolute(workspace, path) : path
    if (relative === undefined) return
    await this.openFile(relative)
    if (this.state.getSnapshot().workspace?.workspaceId !== workspace.workspaceId) return
    this.patch({ reveal: { path: relative, line, column } })
  }
  /** Publish a selection that may be explicitly sent to chat. @param selection Current source selection. */
  selection(selection: EditorSelection | undefined): void {
    this.patch({ selection })
  }
  /** Publish diagnostics from language clients. @param problems Current markers. */
  problems(problems: readonly EditorProblem[]): void {
    this.patch({ problems })
  }
  /** Publish one language-client status. @param language Language. @param phase Connection phase. @param message Failure detail. */
  language(language: string, phase: string, message?: string): void {
    this.patch({ languageStates: { ...this.state.getSnapshot().languageStates, [language]: message ?? phase } })
  }
  /** Retain a visible operation error. @param error Operation failure. */
  fail(error: unknown): void {
    if (!this.disposed)
      this.patch({
        phase: this.switching ? 'loading' : this.state.getSnapshot().workspace === null ? 'error' : 'ready',
        error: this.options.describeError?.(error) ?? (error instanceof Error ? error.message : String(error)),
      })
  }

  /**
   * Durably save all recovery buffers; false blocks workspace change and application quit.
   * @returns Whether every observed edit was persisted.
   */
  async flush(): Promise<boolean> {
    if (this.timer !== undefined) clearTimeout(this.timer)
    this.timer = undefined
    if (this.persistence !== undefined) {
      if (!(await this.persistence)) return false
      return this.flush()
    }
    if (this.state.getSnapshot().recoveryConflict)
      return Object.values(this.state.getSnapshot().buffers).every(buffer => !buffer.dirty)
    if (this.state.getSnapshot().workspace === null || this.generation === this.persistedGeneration) return true
    this.persistence = this.persist()
    const saved = await this.persistence
    this.persistence = undefined
    return saved && (this.generation === this.persistedGeneration || (await this.flush()))
  }

  /** Stop future observations; caller flushes before disposing the model. */
  dispose(): void {
    this.disposed = true
    if (this.timer !== undefined) clearTimeout(this.timer)
    if (this.polling !== undefined) clearTimeout(this.polling)
  }

  private patch(patch: Partial<IdeState>): void {
    if (!this.disposed) this.state.set({ ...this.state.getSnapshot(), ...patch })
  }
  private isDisposed(): boolean {
    return this.disposed
  }
  private setData(data: Partial<IdeWorkspaceStateData>): void {
    this.patch({ data: { ...this.state.getSnapshot().data, ...data } })
    this.changed()
  }
  private changed(): void {
    this.generation++
    if (this.timer !== undefined) clearTimeout(this.timer)
    this.timer = setTimeout(() => {
      this.timer = undefined
      void this.flush()
    }, this.options.debounceMs)
  }
  private async persist(): Promise<boolean> {
    const current = this.state.getSnapshot()
    if (current.workspace === null) return true
    const generation = this.generation
    const data: IdeWorkspaceStateData = {
      ...current.data,
      tabs: current.data.tabs.filter(tab => current.buffers[tab.path]?.source !== 'snippet'),
      activePath:
        current.data.activePath !== null && current.buffers[current.data.activePath]?.source === 'snippet'
          ? (current.data.tabs.find(tab => current.buffers[tab.path]?.source !== 'snippet')?.path ?? null)
          : current.data.activePath,
      buffers: Object.entries(current.buffers)
        .filter(([, buffer]) => buffer.dirty)
        .map(([path, buffer]) => ({
          path,
          content: buffer.text,
          baseVersion: buffer.document.version,
          bom: buffer.document.bom,
          eol: buffer.document.eol,
        })),
    }
    this.patch({ saving: true })
    try {
      const saved: IdeWorkspaceState = await this.api.request({
        op: 'state.save',
        workspaceId: current.workspace.workspaceId,
        baseRevision: this.revision,
        data,
      })
      this.revision = saved.revision
      this.persistedGeneration = generation
      return true
    } catch (error) {
      this.fail(error)
      if (error instanceof IdeRequestError && error.code === 'revision-conflict') this.patch({ recoveryConflict: true })
      return false
    } finally {
      this.patch({ saving: false })
    }
  }
  private schedulePoll(): void {
    if (this.disposed) return
    this.polling = setTimeout(() => {
      void this.poll().finally(() => {
        this.schedulePoll()
      })
    }, this.options.pollMs)
  }
  private async poll(): Promise<void> {
    const before = this.state.getSnapshot()
    if (before.workspace === null || this.switching || before.data.tabs.length === 0) return
    try {
      const changes = await this.api.request({
        op: 'files.changes',
        workspaceId: before.workspace.workspaceId,
        paths: before.data.tabs.filter(tab => before.buffers[tab.path]?.source !== 'snippet').map(tab => tab.path),
      })
      if (this.state.getSnapshot().workspace?.workspaceId !== before.workspace.workspaceId) return
      for (const change of changes) {
        const current = this.state.getSnapshot()
        const buffer = current.buffers[change.path]
        if (buffer === undefined || change.version === buffer.document.version) continue
        if (buffer.dirty || change.version === null)
          this.patch({ buffers: { ...current.buffers, [change.path]: { ...buffer, external: true } } })
        else await this.reload(change.path)
      }
    } catch (error) {
      this.fail(error)
    }
  }
}
