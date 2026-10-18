/** Translate renderer file keys at the typed IDE protocol, retaining primary-root compatibility, and give the page file
 *  text with the line ends the editor holds. */
import type { IdeFileDiff, IdeFileDocument, IdeWorkspaceStateData, IdeWorkspaceState } from '../../shared/ide-files-protocol.ts'
import type { IdeFilesApi, IdeApiRequest, IdeApiResults } from './ide-api.ts'
import { IdeRequestError } from './ide-api.ts'
import { fileKey, fileReference } from './ide-paths.ts'

/**
 * File text as the editor holds it. CodeMirror splits lines at CRLF, CR and LF and joins them with LF, so text compared
 * with the editor's must too; the Host writes a CRLF file back with CRLF.
 * @param text File text.
 * @returns The text with LF line ends.
 */
export function editorText(text: string): string {
  return text.includes('\r') ? text.replace(/\r\n?/gu, '\n') : text
}

function editorDocument(document: IdeFileDocument): IdeFileDocument {
  return document.content === null ? document : { ...document, content: editorText(document.content) }
}

function editorDiff(diff: IdeFileDiff): IdeFileDiff {
  return { ...diff, base: diff.base === null ? null : editorText(diff.base), current: diff.current === null ? null : editorText(diff.current) }
}

function decodeState(state: IdeWorkspaceState): IdeWorkspaceState {
  const data = state.data
  return { ...state, data: { ...data,
    tabs: data.tabs.map(entry => ({ ...entry, rootId: undefined, path: fileKey(entry.path, entry.rootId) })),
    // Drafts saved before the line ends were normalized compare equal to the file again.
    buffers: data.buffers.map(entry => ({ ...entry, content: editorText(entry.content), rootId: undefined, path: fileKey(entry.path, entry.rootId) })),
    activePath: data.activePath === null ? null : fileKey(data.activePath, data.activeRootId), activeRootId: undefined,
    expandedPaths: [...data.expandedPaths, ...data.expandedRoots?.map(entry => fileKey(entry.path, entry.rootId)) ?? []],
    expandedRoots: undefined,
  } }
}

function encodeState(data: IdeWorkspaceStateData): IdeWorkspaceStateData {
  const active = data.activePath === null ? undefined : fileReference(data.activePath)
  const expanded = data.expandedPaths.map(fileReference)
  return { ...data,
    tabs: data.tabs.map(entry => ({ ...entry, ...fileReference(entry.path) })),
    buffers: data.buffers.map(entry => ({ ...entry, ...fileReference(entry.path) })),
    activePath: active?.path ?? null, activeRootId: active?.rootId,
    expandedPaths: expanded.filter(entry => entry.rootId === undefined).map(entry => entry.path),
    expandedRoots: expanded.filter(entry => entry.rootId !== undefined),
  }
}

/** Preserve the original file/state API while qualifying file identities in renderer memory.
 * @param api Validated wire API.
 * @returns An adapter whose file paths are renderer keys.
 */
export function createRootedIdeApi(api: IdeFilesApi): IdeFilesApi {
  const wire: IdeFilesApi['request'] = (body, signal) => signal === undefined ? api.request(body) : api.request(body, signal)
  const request = async (body: IdeApiRequest, signal?: AbortSignal): Promise<IdeApiResults[keyof IdeApiResults]> => {
    if (body.op === 'state.read' || body.op === 'state.save') {
      try {
        const state = await wire(body.op === 'state.save' ? { ...body, data: encodeState(body.data) } : body, signal)
        return decodeState(state)
      } catch (error) {
        if (error instanceof IdeRequestError && error.currentState !== undefined)
          throw new IdeRequestError(error.code, error.message, decodeState(error.currentState))
        throw error
      }
    }
    if (body.op === 'files.changes') {
      const groups = new Map<ReturnType<typeof fileReference>['rootId'], string[]>()
      for (const path of body.paths) {
        const root = fileReference(path).rootId
        groups.set(root, [...groups.get(root) ?? [], path])
      }
      const changes = await Promise.all([...groups].map(async ([rootId, paths]) => {
        const result = await wire({ ...body, ...(rootId === undefined ? {} : { rootId }),
          paths: paths.map(path => fileReference(path).path) }, signal)
        return result.map(change => ({ ...change, path: fileKey(change.path, rootId) }))
      }))
      return changes.flat()
    }
    if (body.op === 'files.list' || body.op === 'files.read' || body.op === 'files.save' || body.op === 'files.create'
      || body.op === 'files.mkdir' || body.op === 'files.rename' || body.op === 'files.deletePreview'
      || body.op === 'files.delete' || body.op === 'files.diff' || body.op === 'format') {
      const reference = fileReference(body.path)
      const location = reference.rootId === undefined ? { path: reference.path } : reference
      if (body.op === 'files.rename') {
        const destination = fileReference(body.destination)
        if (destination.rootId !== reference.rootId)
          throw new IdeRequestError('invalid-path', 'Moving files between project roots is not supported.')
        const result = await wire({ ...body, ...location, destination: destination.path }, signal)
        return { ...result, path: fileKey(result.path, reference.rootId) }
      }
      if (body.op === 'files.list') {
        const result = await wire({ ...body, ...location }, signal)
        return { ...result, path: fileKey(result.path, reference.rootId),
          entries: result.entries.map(entry => ({ ...entry, path: fileKey(entry.path, reference.rootId) })) }
      }
      // The formatter reads only the file name, and its request carries no root identity.
      if (body.op === 'format') return wire({ ...body, path: reference.path }, signal)
      const result = await wire({ ...body, ...location }, signal)
      if (body.op === 'files.read' || body.op === 'files.save' || body.op === 'files.create') {
        return { ...editorDocument(result as IdeFileDocument), path: fileKey(result.path, reference.rootId) }
      }
      if (body.op === 'files.diff') return { ...editorDiff(result as IdeFileDiff), path: fileKey(result.path, reference.rootId) }
      return { ...result, path: fileKey(result.path, reference.rootId) }
    }
    return wire(body, signal)
  }
  return {
    request: async <K extends IdeApiRequest['op']>(body: Extract<IdeApiRequest, { op: K }>, signal?: AbortSignal) =>
      // Each branch changes only path spelling; the operation's result fields are retained.
      await request(body, signal) as IdeApiResults[K],
  }
}
