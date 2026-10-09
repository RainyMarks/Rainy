/** Typed and validated same-origin API for human terminals, program runs and launch-only debugging. */
import { z } from 'zod'
import { brandString } from '@deepseek-ai/dsh-brand'
import type { WorkspaceId, IdeRootId } from '../ide-files-protocol.ts'
import type {
  IdeDebugId, IdeDebugSnapshot, IdeExecutionEvent, IdeExecutionRequest, IdeExecutionResponseMap,
  IdeOperationId, IdeRunId, IdeRunSnapshot, IdeTerminalId, IdeTerminalSnapshot,
} from '../ide-execution-protocol.ts'
import { postIde } from './ide-api.ts'

export { executionConfigurationSchema } from './ide-execution-schema.ts'

const reference = z.number().int().nonnegative()
const workspaceId = z.string().min(1).transform(value => brandString<WorkspaceId>(value))
const rootId = z.string().min(1).transform(value => brandString<IdeRootId>(value))
const runId = z.string().min(1).transform(value => brandString<IdeRunId>(value))
const terminalId = z.string().min(1).transform(value => brandString<IdeTerminalId>(value))
const debugId = z.string().min(1).transform(value => brandString<IdeDebugId>(value))
const operationId = z.string().min(1).transform(value => brandString<IdeOperationId>(value))
const language = z.enum(['python', 'javascript', 'typescript', 'php', 'c', 'cpp'])
const exit = z.object({ exitCode: z.number().int().nullable(), signal: z.string().nullable(), stopped: z.boolean() })
const command = z.object({ argv: z.array(z.string()), cwd: z.string(), environment: z.record(z.string(), z.string()) })
const runSpec = z.object({ workspaceId, rootId: rootId.optional(), workspaceRoot: z.string(),
  language, program: z.string(), name: z.string(),
  launch: command, build: z.array(command), buildDirectory: z.string().optional(), terminal: z.boolean() })
const runSchema: z.ZodType<IdeRunSnapshot> = z.object({ id: runId, workspaceId, name: z.string(),
  phase: z.enum(['starting', 'building', 'running', 'stopping', 'exited', 'failed']), spec: runSpec,
  exit: exit.optional(), error: z.string().optional() })
const terminalSchema: z.ZodType<IdeTerminalSnapshot> = z.object({ id: terminalId, workspaceId, cwd: z.string(),
  phase: z.enum(['starting', 'running', 'stopping', 'exited', 'failed']), exit: exit.optional(), error: z.string().optional() })
const breakpoint = z.object({ id: reference.optional(), path: z.string(), requestedLine: reference.optional(), line: reference.optional(),
  verified: z.boolean(), message: z.string().optional() })
const debugSchema: z.ZodType<IdeDebugSnapshot> = z.object({ id: debugId, workspaceId, rootId: rootId.optional(), name: z.string(), language,
  phase: z.enum(['starting', 'building', 'initializing', 'running', 'paused', 'stopping', 'terminated', 'failed']),
  threadId: reference.optional(), reason: z.string().optional(), breakpoints: z.array(breakpoint),
  capabilities: z.object({ configurationDone: z.boolean(), evaluate: z.boolean(), pause: z.boolean(), stepIn: z.boolean(),
    stepOut: z.boolean(), next: z.boolean() }).optional(), exit: exit.optional(), error: z.string().optional() })
const status = z.object({ runs: z.array(runSchema), terminals: z.array(terminalSchema), debugSessions: z.array(debugSchema) })
const eventBase = { sequence: reference, workspaceId }
const event: z.ZodType<IdeExecutionEvent> = z.discriminatedUnion('kind', [
  z.object({ ...eventBase, kind: z.literal('output'), operationId, stream: z.enum(['stdout', 'stderr', 'terminal', 'adapter']), text: z.string() }),
  z.object({ ...eventBase, kind: z.literal('run'), run: runSchema }),
  z.object({ ...eventBase, kind: z.literal('terminal'), terminal: terminalSchema }),
  z.object({ ...eventBase, kind: z.literal('debug'), debug: debugSchema }),
  z.object({ ...eventBase, kind: z.literal('breakpoints'), debugId, breakpoints: z.array(breakpoint) }),
])
const childCounts = { namedVariables: reference.optional(), indexedVariables: reference.optional() }
const evaluation = z.object({ result: z.string(), type: z.string().optional(), variablesReference: reference, ...childCounts })
const done = z.object({ ok: z.literal(true) })

const schemas: { [K in keyof IdeExecutionResponseMap]: z.ZodType<IdeExecutionResponseMap[K]> } = {
  'execution.status': status,
  'execution.poll': z.object({ events: z.array(event), cursor: reference, truncated: z.boolean(), status }),
  'terminal.start': terminalSchema,
  'terminal.input': done,
  'terminal.resize': done,
  'terminal.stop': done,
  'run.resolve': runSpec,
  'run.start': runSchema,
  'run.input': done,
  'run.resize': done,
  'run.stop': done,
  'debug.start': debugSchema,
  'debug.setBreakpoints': z.array(breakpoint),
  'debug.threads': z.array(z.object({ id: reference, name: z.string() })),
  'debug.stack': z.array(z.object({ id: reference, name: z.string(), path: z.string().optional(), line: reference, column: reference })),
  'debug.scopes': z.array(z.object({ name: z.string(), variablesReference: reference, expensive: z.boolean(), ...childCounts })),
  'debug.variables': z.array(z.object({ name: z.string(), value: z.string(), type: z.string().optional(), variablesReference: reference,
    ...childCounts, evaluateName: z.string().optional() })),
  'debug.evaluate': evaluation,
  'debug.control': done,
  'debug.input': done,
  'debug.resize': done,
  'debug.stop': done,
}

/** Operation-specific typed API consumed by the React-free execution model. */
export interface IdeExecutionApi {
  /** @param request - human operation. @param signal - optional request cancellation. @returns its validated value. */
  request<K extends IdeExecutionRequest['op']>(
    request: Extract<IdeExecutionRequest, { op: K }>, signal?: AbortSignal,
  ): Promise<IdeExecutionResponseMap[K]>
}

/**
 * Construct the authenticated same-origin execution API; responses are parsed before publication.
 * @returns a stateless typed adapter over POST /rainy/ide.
 */
export function createIdeExecutionApi(): IdeExecutionApi {
  return { request: <K extends IdeExecutionRequest['op']>(request: Extract<IdeExecutionRequest, { op: K }>, signal?: AbortSignal) =>
    postIde(request, schemas[request.op], signal) }
}
