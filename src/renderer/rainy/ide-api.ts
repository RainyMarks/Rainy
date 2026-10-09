/** Validated browser requests for workspace files, recovery state, and formatting. */
import { z } from 'zod'
import { executionConfigurationSchema } from './ide-execution-schema.ts'
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import type {
  IdeDeleteToken,
  IdeFilesRequest,
  IdeFilesResults,
  IdeFileVersion,
  IdeStateRequest,
  IdeRootId,
  WorkspaceId,
} from '../ide-files-protocol.ts'

const workspaceId = z
  .string()
  .min(1)
  .transform(value => value as WorkspaceId)
const version = z
  .string()
  .min(1)
  .transform(value => value as IdeFileVersion)
const rootId = z.string().min(1).max(128).transform(value => value as IdeRootId)
const entryKind = z.enum(['file', 'directory', 'symlink', 'other'])
const position = z.object({ line: z.number().int().positive(), column: z.number().int().positive() })
const documentSchema = z.object({
  workspaceId,
  path: z.string(),
  version,
  bytes: z.number().nonnegative(),
  content: z.string().nullable(),
  bom: z.boolean(),
  eol: z.enum(['lf', 'crlf', 'mixed']),
  readOnlyReason: z.enum(['binary', 'too-large', 'unsupported-encoding']).nullable(),
  preview: z
    .object({
      kind: z.enum(['utf8', 'hex']),
      text: z.string(),
      bytesRead: z.number().int().nonnegative(),
      truncated: z.boolean(),
    })
    .optional(),
})
const entrySchema = z.object({
  name: z.string(),
  path: z.string(),
  kind: entryKind,
  bytes: z.number().nonnegative(),
  version,
  outsideWorkspace: z.boolean(),
  targetKind: z.enum(['file', 'directory', 'other']).optional(),
})
const workspaceSchema = z.object({ workspaceId, path: z.string(), title: z.string(),
  roots: z.array(z.object({ rootId, path: z.string(), title: z.string(), primary: z.boolean() })).optional() })
const selectionSchema = z.object({ version: z.literal(1), workspaceId: workspaceId.nullable() })
const layoutSchema = z.object({
  sidebarWidth: z.number().nonnegative(),
  agentWidth: z.number().nonnegative(),
  bottomHeight: z.number().nonnegative(),
  sidebarVisible: z.boolean(),
  agentVisible: z.boolean(),
  bottomVisible: z.boolean(),
  bottomTab: z.enum(['terminal', 'problems', 'output', 'debug']),
})
const stateSchema = z.object({
  version: z.literal(1),
  revision: z.number().int().nonnegative(),
  data: z.object({
    tabs: z.array(
      z.object({
        path: z.string(),
        rootId: rootId.optional(),
        kind: z.enum(['file', 'diff']),
        cursor: position.optional(),
        scroll: z.object({ top: z.number().nonnegative(), left: z.number().nonnegative() }).optional(),
      }),
    ),
    activePath: z.string().nullable(),
    activeRootId: rootId.optional(),
    expandedPaths: z.array(z.string()),
    expandedRoots: z.array(z.object({ rootId: rootId.optional(), path: z.string() })).optional(),
    buffers: z.array(
      z.object({
        path: z.string(),
        rootId: rootId.optional(),
        content: z.string(),
        baseVersion: version.nullable(),
        bom: z.boolean(),
        eol: z.enum(['lf', 'crlf', 'mixed']),
      }),
    ),
    layout: layoutSchema,
    lastSessionId: z
      .string()
      .transform(value => value as SessionId)
      .nullable(),
    execution: executionConfigurationSchema.optional(),
  }),
})

/** Formatter request served by the same authenticated IDE dispatcher. */
export interface IdeFormatRequest {
  readonly op: 'format'
  readonly workspaceId: WorkspaceId
  readonly rootId?: IdeRootId | undefined
  readonly path: string
  readonly text: string
  readonly language: string
}

/** Results accepted by the editor's private file/state adapter. */
export type IdeApiResults = IdeFilesResults & { format: { readonly text: string } }
/** Requests accepted by the editor's private file/state adapter. */
export type IdeApiRequest = IdeFilesRequest | IdeStateRequest | IdeFormatRequest
type Results = IdeApiResults
type Request = IdeApiRequest

const schemas: { [K in keyof Results]: z.ZodType<Results[K]> } = {
  'workspaces.list': z.array(workspaceSchema),
  'workspaces.open': workspaceSchema,
  'workspaces.attach': workspaceSchema,
  'workspaces.removeRoot': workspaceSchema,
  'files.list': z.object({ path: z.string(), entries: z.array(entrySchema) }),
  'files.search': z.object({ paths: z.array(z.string()), truncated: z.boolean() }),
  'files.read': documentSchema,
  'files.save': documentSchema,
  'files.create': documentSchema,
  'files.mkdir': entrySchema,
  'files.rename': entrySchema,
  'files.deletePreview': z.object({
    path: z.string(),
    kind: entryKind,
    entries: z.number().int().nonnegative(),
    bytes: z.number().nonnegative(),
    token: z
      .string()
      .min(1)
      .transform(value => value as IdeDeleteToken),
  }),
  'files.delete': z.object({ path: z.string(), deleted: z.literal(true) }),
  'files.diff': z.object({
    path: z.string(),
    base: z.string().nullable(),
    current: z.string().nullable(),
    version: version.nullable(),
    status: z.enum(['modified', 'added', 'deleted', 'unchanged', 'unavailable']),
    reason: z.enum(['not-git', 'binary', 'too-large', 'unsupported-encoding']).optional(),
  }),
  'files.changes': z.array(
    z.object({ path: z.string(), version: version.nullable(), kind: z.enum([...entryKind.options, 'missing']) }),
  ),
  'state.read': stateSchema,
  'state.save': stateSchema,
  'state.selection.read': selectionSchema,
  'state.selection.save': selectionSchema,
  format: z.object({ text: z.string() }),
}

const envelopeSchema = z.discriminatedUnion('ok', [
  z.object({ ok: z.literal(true), value: z.unknown() }),
  z.object({
    ok: z.literal(false),
    error: z.object({
      code: z.string(),
      message: z.string(),
      currentVersion: version.nullable().optional(),
      currentState: stateSchema.optional(),
    }),
  }),
])

/** Host failures retain their stable code and optional current revision for recovery. */
export class IdeRequestError extends Error {
  /**
   * @param code Stable Host error identifier.
   * @param message Host diagnostic.
   * @param currentState Conflicting durable workspace state.
   */
  constructor(
    readonly code: string,
    message: string,
    readonly currentState?: IdeFilesResults['state.read'],
  ) {
    super(message)
    this.name = 'IdeRequestError'
  }
}

/** Validate one response through its operation-owned parser.
 * @param body JSON-compatible IDE request.
 * @param schema Parser for the successful value.
 * @param signal Optional cancellation for the request and response body.
 * @returns The validated response value.
 */
export async function postIde<T>(body: object, schema: z.ZodType<T>, signal?: AbortSignal): Promise<T> {
  const response = await fetch('/rainy/ide', {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
    ...(signal === undefined ? {} : { signal }),
  })
  const raw: unknown = await response.json()
  const envelope = envelopeSchema.safeParse(raw)
  if (!envelope.success) throw new IdeRequestError('invalid-response', 'The IDE response is invalid.')
  if (!envelope.data.ok) {
    const failure = envelope.data.error
    throw new IdeRequestError(failure.code, failure.message, failure.currentState)
  }
  if (!response.ok) throw new IdeRequestError('request-failed', `IDE request failed with HTTP ${response.status}.`)
  const parsed = schema.safeParse(envelope.data.value)
  if (!parsed.success) throw new IdeRequestError('invalid-response', 'The IDE response fields are invalid.')
  return parsed.data
}

/** Typed file/state adapter; Host responses are validated before entering editor state. */
export interface IdeFilesApi {
  /** @param request Workspace operation. @param signal Optional cancellation. @returns Its validated result. */
  request<K extends Request['op']>(request: Extract<Request, { op: K }>, signal?: AbortSignal): Promise<Results[K]>
}

/** Create the same-origin file/state adapter used by the installed workspace.
 * @returns A stateless validated API facade.
 */
export function createIdeFilesApi(): IdeFilesApi {
  return {
    request: <K extends Request['op']>(request: Extract<Request, { op: K }>, signal?: AbortSignal) =>
      postIde(request, schemas[request.op], signal),
  }
}
