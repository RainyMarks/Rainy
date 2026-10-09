/** Retained editor mount; editor buffers remain owned by the workspace model. */
import { useEffect, useRef, useState } from 'react'
import type { ReactNode } from 'react'
import type { EditorAppearance, EditorInstance } from './editor-types.ts'
import { loadEditorAssets } from './editor-loader.ts'
import { sourceLanguage, sourceUri, type IdeModel, type IdeState } from './ide-model.ts'
import { fileKey, workspaceRoots } from './ide-paths.ts'
import { selectedPythonExecutable } from './run-target.ts'
import { useIdeT } from './messages.ts'
import { useLocale } from '../i18n.ts'
import css from './Ide.module.css'

interface Props {
  readonly model: IdeModel
  readonly state: IdeState
  readonly appearance: EditorAppearance
  readonly sendSelection: () => void
  readonly breakpoint: (path: string, line: number) => void
  readonly attach: (editor: EditorInstance | undefined) => void
  readonly stopped?: { readonly path: string; readonly line: number } | undefined
  /** Shown instead of the editor while no file is open. */
  readonly empty?: ReactNode
}

/** Keep one editor instance while tabs, source text, and window geometry change.
 * @param props The workspace model, appearance and editor callbacks.
 * @returns The editor mount and loading state.
 */
export function IdeEditor({ model, state, appearance, sendSelection, breakpoint, attach, stopped, empty }: Props) {
  const t = useIdeT()
  const locale = useLocale()
  const container = useRef<HTMLDivElement>(null)
  const instance = useRef<EditorInstance | undefined>(undefined)
  const revealed = useRef<IdeState['reveal']>()
  const callbacks = useRef({ sendSelection, breakpoint, attach })
  callbacks.current = { sendSelection, breakpoint, attach }
  const labels = useRef({ save: t('ideSave'), format: t('ideFormat'), sendSelection: t('ideSendSelection'), toggleBreakpoint: t('ideToggleBreakpoint'),
    gotoDefinition: t('ideGotoDefinition'), findReferences: t('ideFindReferences'), rename: t('ideRenameSymbol'), locale })
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
        .filter(([, buffer]) => buffer.document.content !== null)
        .flatMap(([path, buffer]) => {
          const uri = buffer.source === 'snippet' ? `untitled:${encodeURIComponent(path)}`
            : workspace === null ? undefined : sourceUri(workspace, path)
          if (uri === undefined) return []
          return [{
            path,
            uri,
            language: buffer.language ?? sourceLanguage(path),
            text: buffer.text,
            readOnly: buffer.source === 'snippet' || buffer.document.readOnlyReason !== null || state.phase === 'loading',
          }]
        }),
    )
  }, [workspaceReady, state.workspace, state.buffers, state.phase])
  const path = state.data.activePath
  const tab = state.data.tabs.find(entry => entry.path === path)
  const buffer = path === null ? undefined : state.buffers[path]
  const comparison = buffer?.comparison
  useEffect(() => {
    if (workspaceReady === undefined || path === null) return
    const editor = instance.current
    if (tab?.kind === 'diff' && comparison !== undefined)
      editor?.showDiff(path, comparison.original, buffer?.source !== 'snippet')
    else
      editor?.show(path, tab?.cursor === undefined ? undefined : { ...tab.cursor, top: tab.scroll?.top ?? 0, left: tab.scroll?.left ?? 0 })
  }, [workspaceReady, path, tab?.kind, comparison])
  useEffect(() => {
    instance.current?.setAppearance(appearance)
  }, [ready, appearance])
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
  }, [state.center, state.data.layout])
  const reason = buffer?.document.readOnlyReason
  return (
    <div className={css.editorHost}>
      <div ref={container} className={css.editor} hidden={path === null || (reason !== null && reason !== undefined)} data-rainy-editor />
      {!ready && path !== null && <div className={css.empty} role="status">{t('ideEditorLoading')}</div>}
      {path === null && (empty ?? <div className={css.empty}>{t(state.workspace === null ? 'ideNoWorkspace' : 'ideNoFile')}</div>)}
      {buffer !== undefined && reason !== null && reason !== undefined && (
        <div className={css.empty}>
          <span>{t(reason === 'binary' ? 'ideReadOnlyBinary' : reason === 'too-large' ? 'ideReadOnlyLarge' : 'ideReadOnlyEncoding')}</span>
          <small>{t('ideSize', { bytes: buffer.document.bytes })}</small>
          {buffer.document.preview !== undefined && (
            <>
              <small>
                {t('ideReadOnlyPreview', { read: buffer.document.preview.bytesRead, total: buffer.document.bytes })}
                {buffer.document.preview.truncated ? ` · ${t('idePreviewTruncated')}` : ''}
              </small>
              <pre className={css.preview} aria-label={t('ideReadOnlyPreview', { read: buffer.document.preview.bytesRead, total: buffer.document.bytes })}>
                {buffer.document.preview.text}
              </pre>
            </>
          )}
        </div>
      )}
    </div>
  )
}
