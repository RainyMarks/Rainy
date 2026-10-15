/** Retained editor mount and the file viewers beside it; editor buffers remain owned by the workspace model. */
import { lazy, Suspense, useEffect, useMemo, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import type { EditorActionLabels, EditorAppearance, EditorInstance } from './editor-types.ts'
import { loadEditorAssets } from './editor-loader.ts'
import { rewrite, suggest } from './ai-assist.ts'
import { usePrefs } from '../prefs.ts'
import { MAX_DECODE_BYTES, sourceLanguage, sourceUri, type IdeModel, type IdeState } from './ide-model.ts'
import { fileKey, workspaceRoots } from './ide-paths.ts'
import { selectedPythonExecutable } from './run-target.ts'
import { useIdeT } from './messages.ts'
import { useLocale } from '../i18n.ts'
import { useReviewT } from '../review/messages.ts'
import { useViewerT } from './viewers/messages.ts'
import { ScrollSync } from './viewers/scroll-sync.ts'
import { chooseViewer, TEXT_ENCODINGS } from './viewers/viewer-choice.ts'
import css from './Ide.module.css'
import viewerCss from './viewers/Viewers.module.css'

/** Viewers load with the first tab that needs one, outside the main bundle. */
const FileViewer = lazy(async () => ({ default: (await import('./viewers/FileViewer.tsx')).FileViewer }))

interface Props {
  readonly model: IdeModel
  readonly state: IdeState
  readonly appearance: EditorAppearance
  readonly sendSelection: () => void
  readonly breakpoint: (path: string, line: number) => void
  readonly attach: (editor: EditorInstance | undefined) => void
  /** A review of the agent's changes kept or undid a change; `remaining` changes are left. */
  readonly review?: ((path: string, remaining: number) => void) | undefined
  readonly stopped?: { readonly path: string; readonly line: number } | undefined
  /** Shown instead of the editor while no file is open. */
  readonly empty?: ReactNode
}

/** Keep one editor instance while tabs, source text, and window geometry change.
 * @param props The workspace model, appearance and editor callbacks.
 * @returns The editor mount, the viewer of the active tab and loading state.
 */
export function IdeEditor({ model, state, appearance, sendSelection, breakpoint, attach, review, stopped, empty }: Props) {
  const t = useIdeT()
  const tr = useReviewT()
  const tv = useViewerT()
  const locale = useLocale()
  const container = useRef<HTMLDivElement>(null)
  const instance = useRef<EditorInstance | undefined>(undefined)
  const revealed = useRef<IdeState['reveal']>()
  const callbacks = useRef({ sendSelection, breakpoint, attach, review })
  callbacks.current = { sendSelection, breakpoint, attach, review }
  const scrollSync = useMemo(() => new ScrollSync(), [])
  const activePath = useRef<string | null>(null)
  const actionLabels: EditorActionLabels = { save: t('ideSave'), format: t('ideFormat'), sendSelection: t('ideSendSelection'),
    toggleBreakpoint: t('ideToggleBreakpoint'), gotoDefinition: t('ideGotoDefinition'), findReferences: t('ideFindReferences'),
    rename: t('ideRenameSymbol'), aiEdit: t('ideAiEdit'), aiEditPlaceholder: t('ideAiEditPlaceholder'), aiEditGenerate: t('ideAiEditGenerate'),
    aiEditGenerating: t('ideAiEditGenerating'), aiEditStop: t('ideAiEditStop'), aiEditAccept: t('ideAiEditAccept'), aiEditReject: t('ideAiEditReject'),
    aiEditReview: t('ideAiEditReview'), aiEditUnchanged: t('ideAiEditUnchanged'), aiEditUnavailable: t('ideAiEditUnavailable'),
    reviewKeep: tr('reviewKeep'), reviewUndo: tr('reviewUndo'), reviewKeepTitle: tr('reviewKeepTitle'), reviewUndoTitle: tr('reviewUndoTitle'), locale }
  const labels = useRef(actionLabels)
  labels.current = actionLabels
  const [ready, setReady] = useState(false)
  const [workspaceReady, setWorkspaceReady] = useState<string | undefined>()
  const pythonPath = selectedPythonExecutable(state.data.execution)
  const profile = state.data.execution?.profiles.find(entry => entry.name === state.data.execution?.activeProfile)
  const compiled =
    profile?.language === 'c' || profile?.language === 'cpp'
      ? profile
      : state.data.execution?.profiles.find(entry => entry.language === 'c' || entry.language === 'cpp')
  const compileCommandsDirectory = compiled?.build?.kind === 'cmake' ? compiled.build.buildDirectory : undefined
  useEffect(() => {
    const host = container.current
    if (host === null) return
    let disposed = false
    void loadEditorAssets()
      .then(assets =>
        assets.create(
          host,
          {
            change: (path, text) => { model.change(path, text) },
            view: (path, view) => { model.view(path, view) },
            selection: (selection) => { model.selection(selection) },
            problems: (problems) => { model.problems(problems) },
            languageState: (language, phase, message) => { model.language(language, phase, message) },
            open: async (uri, line, column) => {
              const document = await model.openUri(uri, 'open')
              if (document !== undefined && line !== undefined)
                queueMicrotask(() => { instance.current?.reveal(document.path, line, column ?? 1) })
              return document
            },
            read: uri => model.openUri(uri, 'read'),
            prepareEdit: uri => model.openUri(uri, 'edit'),
            save: () => { void model.save() },
            format: () => { void model.format().catch((error: unknown) => { model.fail(error) }) },
            sendSelection: () => { callbacks.current.sendSelection() },
            breakpoint: (path, line) => { callbacks.current.breakpoint(path, line) },
            suggest,
            rewrite,
            scroll: (path, line) => { if (path === activePath.current) scrollSync.editorScrolled(line) },
            review: (path, progress) => {
              model.reviewed(path, progress.original, progress.remaining)
              callbacks.current.review?.(path, progress.remaining)
            },
          },
          labels.current,
        ),
      )
      .then((editor) => {
        if (disposed) {
          void editor.dispose()
          return
        }
        instance.current = editor
        callbacks.current.attach(editor)
        setReady(true)
      })
      .catch((error: unknown) => {
        if (!disposed) model.fail(error)
      })
    return () => {
      disposed = true
      callbacks.current.attach(undefined)
      void instance.current?.dispose()
      instance.current = undefined
    }
  }, [model])
  useEffect(() => {
    const editor = instance.current
    if (!ready || editor === undefined) return
    const workspace = state.workspace
    const identity = workspace?.workspaceId ?? 'rainy:previews'
    let cancelled = false
    setWorkspaceReady(undefined)
    void model
      .flush()
      .then(async (saved) => {
        if (!saved) throw new Error(t('ideRecoveryConflict'))
        await editor.setWorkspace({
          id: identity,
          path: workspace?.path ?? '/',
          title: workspace?.title ?? t('ideAppTitle'),
          roots: workspace === null ? [] : workspaceRoots(workspace),
          pythonPath,
          compileCommandsDirectory,
        })
      })
      .then(() => {
        if (!cancelled) setWorkspaceReady(identity)
      })
      .catch((error: unknown) => {
        if (!cancelled) model.fail(error)
      })
    return () => {
      cancelled = true
    }
  }, [ready, state.workspace, pythonPath, compileCommandsDirectory, model])
  useEffect(() => {
    const workspace = state.workspace
    if (workspaceReady !== (workspace?.workspaceId ?? 'rainy:previews')) return
    instance.current?.updateDocuments(
      Object.entries(state.buffers)
        .filter(([, buffer]) => buffer.document.content !== null || buffer.decoded !== undefined)
        .flatMap(([path, buffer]) => {
          const uri = buffer.source === 'snippet' ? `untitled:${encodeURIComponent(path)}`
            : workspace === null ? undefined : sourceUri(workspace, path)
          if (uri === undefined) return []
          return [{
            path,
            uri,
            language: buffer.language ?? sourceLanguage(path),
            text: buffer.decoded?.text ?? buffer.text,
            readOnly: buffer.source === 'snippet' || buffer.document.readOnlyReason !== null || state.phase === 'loading',
          }]
        }),
    )
  }, [workspaceReady, state.workspace, state.buffers, state.phase])
  const path = state.data.activePath
  activePath.current = path
  const tab = state.data.tabs.find(entry => entry.path === path)
  const buffer = path === null ? undefined : state.buffers[path]
  const comparison = buffer?.comparison
  const reason = buffer?.document.readOnlyReason
  const decoded = buffer?.decoded
  const textReady = buffer !== undefined && (buffer.document.content !== null || decoded !== undefined)
  const choice = path === null || buffer === undefined ? undefined : chooseViewer(path, reason ?? null, tab?.view, decoded !== undefined)
  const editorVisible = path !== null && textReady && choice?.editor !== false
  useEffect(() => {
    if (workspaceReady === undefined || path === null || !textReady) return
    const editor = instance.current
    if (tab?.kind === 'diff' && comparison !== undefined)
      editor?.showDiff(path, comparison.original, buffer?.source !== 'snippet', comparison.kind === 'ai')
    else
      editor?.show(path, tab?.cursor === undefined ? undefined : { ...tab.cursor, top: tab.scroll?.top ?? 0, left: tab.scroll?.left ?? 0 })
  }, [workspaceReady, path, tab?.kind, comparison, textReady])
  useEffect(() => {
    // Text that is not UTF-8 is decoded when its tab shows text: automatically for a failed UTF-8 read, and again after a
    // reload when the user picked an encoding for the file.
    if (path === null || buffer === undefined || decoded !== undefined || tab?.view === 'hex' || buffer.document.bytes > MAX_DECODE_BYTES) return
    if (reason !== 'unsupported-encoding' && !(reason === 'binary' && model.hasEncoding(path))) return
    void model.decodeAs(path).catch((error: unknown) => { model.fail(error) })
  }, [path, buffer?.document.version, reason, decoded, tab?.view, model])
  useEffect(() => {
    instance.current?.setAppearance(appearance)
  }, [ready, appearance])
  useEffect(() => {
    instance.current?.setLabels(labels.current)
  }, [ready, locale])
  const suggestions = usePrefs().inlineCompletion
  useEffect(() => {
    instance.current?.setAssist({ suggestions })
  }, [ready, suggestions])
  useEffect(() => {
    instance.current?.setBreakpoints(state.data.execution?.breakpoints.map(source => ({
      path: fileKey(source.path, source.rootId), lines: source.lines,
    })) ?? [], stopped)
  }, [ready, workspaceReady, state.data.execution?.breakpoints, stopped?.path, stopped?.line])
  useEffect(() => {
    if (workspaceReady !== undefined && state.reveal?.path === path && state.reveal !== revealed.current) {
      instance.current?.reveal(state.reveal.path, state.reveal.line, state.reveal.column)
      revealed.current = state.reveal
    }
  }, [workspaceReady, state.reveal, path])
  useEffect(() => {
    if (state.center === 'editor') instance.current?.layout()
  }, [state.center, state.data.layout, editorVisible, choice?.split])
  return (
    <div className={css.editorHost}>
      {decoded !== undefined && path !== null && choice?.editor === true && (
        <div className={viewerCss.decoded} role="status">
          <span>{tv('textDecoded', { encoding: decoded.encoding.toUpperCase() })}</span>
          <label>{tv('textEncoding')}{' '}
            <select value={decoded.encoding} onChange={(event) => { void model.decodeAs(path, event.target.value).catch((error: unknown) => { model.fail(error) }) }}>
              {[...new Set([decoded.encoding, ...TEXT_ENCODINGS])].map(encoding => <option key={encoding} value={encoding}>{encoding.toUpperCase()}</option>)}
            </select>
          </label>
          {decoded.replaced > 0 && <span>{tv('textReplaced', { count: decoded.replaced })}</span>}
        </div>
      )}
      <div className={css.editorBody} data-split={choice?.split || undefined}>
        <div ref={container} className={css.editor} hidden={!editorVisible} data-rainy-editor />
        {!ready && editorVisible && <div className={css.empty} role="status">{t('ideEditorLoading')}</div>}
        {path === null && (empty ?? <div className={css.empty}>{t(state.workspace === null ? 'ideNoWorkspace' : 'ideNoFile')}</div>)}
        {path !== null && buffer !== undefined && choice?.editor === true && !textReady && (
          <div className={css.empty} role="status">
            {buffer.document.bytes > MAX_DECODE_BYTES ? tv('textTooLarge', { limit: `${MAX_DECODE_BYTES / 1024 / 1024} MB` }) : tv('loading')}
          </div>
        )}
        {path !== null && buffer !== undefined && choice?.viewer !== undefined && state.workspace !== null && (
          <Suspense fallback={<div className={css.empty} role="status">{tv('loading')}</div>}>
            <FileViewer workspace={state.workspace} path={path} buffer={buffer} choice={choice} model={model} sync={scrollSync}
              onScrollLine={(line) => { instance.current?.scrollToLine(path, line) }} />
          </Suspense>
        )}
      </div>
    </div>
  )
}
