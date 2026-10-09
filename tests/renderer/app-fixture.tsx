/** A workbench over spy APIs, rendered at a chosen viewport with one open Python file. */
import { vi } from 'vitest'
import type { IdeDirectoryListing, IdeFileVersion, IdeWorkspace, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { IdeFilesApi } from '../../src/renderer/ide/ide-api.ts'
import type { IdeExecutionApi } from '../../src/renderer/ide/ide-execution-api.ts'
import type { EditorAssets, EditorInstance } from '../../src/renderer/ide/editor-types.ts'
import { createWorkbench, type NativeIdeBridge, type Workbench } from '../../src/renderer/app/workbench.ts'
import { WorkbenchLayout } from '../../src/renderer/app/Workbench.tsx'
import { render, waitFor, type Rendered } from './ide-dom.tsx'

/** Editor bundle stand-in; tests mock `editor-loader.ts` to return it. */
export const assets = { create: vi.fn<EditorAssets['create']>(), version: 1 as const, terminal: vi.fn<EditorAssets['terminal']>() }

/** The fixture's project. */
export const workspace: IdeWorkspace = { workspaceId: 'a' as WorkspaceId, path: '/tmp/project', title: 'Project' }
/** The fixture's second registered project. */
export const other: IdeWorkspace = { workspaceId: 'b' as WorkspaceId, path: '/tmp/other', title: 'Other' }
/** The open file. */
export const document = {
  workspaceId: workspace.workspaceId, path: 'main.py', version: 'v1' as IdeFileVersion, bytes: 7, content: 'x = 1\n',
  bom: false, eol: 'lf' as const, readOnlyReason: null,
}

/** Everything a window test drives. */
export interface WindowFixture {
  readonly workbench: Workbench
  readonly request: ReturnType<typeof vi.fn<IdeFilesApi['request']>>
  readonly executionRequest: ReturnType<typeof vi.fn<IdeExecutionApi['request']>>
  readonly chooseDirectory: ReturnType<typeof vi.fn<NativeIdeBridge['selectDirectory']>>
  readonly listDirectory: ReturnType<typeof vi.fn<(path: string | undefined, signal: AbortSignal) => Promise<IdeDirectoryListing>>>
  readonly editor: EditorInstance
  readonly view: Rendered
  rerender(width: number): void
}

const workbenches: Workbench[] = []

/** Dispose every workbench created by {@link mountWindow}. */
export async function disposeWindows(): Promise<void> {
  await Promise.all(workbenches.splice(0).map(workbench => workbench.dispose()))
}

/**
 * Create a workbench with `main.py` open and render the window.
 * @param width Viewport width.
 * @param options Native picker presence and the remembered-chat check.
 * @returns Handles for driving and observing it.
 */
export async function mountWindow(width = 1440, options: { native?: boolean; sessionInWorkspace?: (sessionId: string) => boolean } = {}): Promise<WindowFixture> {
  const request = vi.fn<IdeFilesApi['request']>()
  // Like the Host adapter, an unanswered request rejects once it is cancelled.
  const executionRequest = vi.fn<IdeExecutionApi['request']>((_request, signal) => new Promise((_resolve, reject) => {
    signal?.addEventListener('abort', () => { reject(new DOMException('cancelled', 'AbortError')) }, { once: true })
  }))
  const chooseDirectory = vi.fn<NativeIdeBridge['selectDirectory']>().mockResolvedValue(null)
  const listDirectory = vi.fn(async (): Promise<IdeDirectoryListing> => ({ path: '/tmp/project', parent: null, roots: ['/'], entries: [] }))
  const workbench = createWorkbench({
    files: { request: request as IdeFilesApi['request'] },
    execution: { request: executionRequest as IdeExecutionApi['request'] },
    native: options.native === false ? undefined : { selectDirectory: chooseDirectory },
    sessionInWorkspace: async sessionId => options.sessionInWorkspace?.(sessionId) ?? true,
  })
  workbenches.push(workbench)
  const model = workbench.model
  model.state.set({
    ...model.state.getSnapshot(),
    workspace,
    workspaces: [workspace, other],
    phase: 'ready',
    data: { ...model.state.getSnapshot().data, tabs: [{ path: 'main.py', kind: 'file' }], activePath: 'main.py' },
    buffers: { 'main.py': { document, text: document.content, dirty: false, external: false } },
  })
  const editor: EditorInstance = {
    setWorkspace: vi.fn(async () => {}),
    updateDocuments: vi.fn(),
    show: vi.fn(),
    showDiff: vi.fn(),
    setAppearance: vi.fn(),
    setLabels: vi.fn(),
    setBreakpoints: vi.fn(),
    reveal: vi.fn(),
    action: vi.fn(async () => {}),
    layout: vi.fn(),
    dispose: vi.fn(async () => {}),
  }
  assets.create.mockResolvedValue(editor)
  const element = (viewport: number) => <WorkbenchLayout workbench={workbench} viewportWidth={viewport} viewportHeight={900}
    listDirectory={listDirectory} createDirectory={async path => ({ path, parent: null, roots: ['/'], entries: [] })} />
  const view = render(element(width))
  await waitFor(() => { if (vi.mocked(editor.setWorkspace).mock.calls.length !== 1) throw new Error('The editor has no workspace yet') })
  return { workbench, request, executionRequest, chooseDirectory, listDirectory, editor, view,
    rerender: (next) => { view.rerender(element(next)) } }
}
