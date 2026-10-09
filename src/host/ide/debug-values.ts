/** Validation of adapter-owned DAP response and event fields used by the human debugger. */
import { z } from 'zod'

const reference = z.number().int().nonnegative()
const source = z.object({ path: z.string().optional() }).loose()
/** Adapter initialization capabilities consumed by this client. */
export const dapCapabilities = z
  .object({ supportsConfigurationDoneRequest: z.boolean().optional(), supportsEvaluateForHovers: z.boolean().optional() })
  .loose()
/** Adapter breakpoint facts; optional locations may differ from requested lines. */
export const dapBreakpoint = z
  .object({
    id: z.number().int().optional(),
    verified: z.boolean(),
    line: z.number().int().positive().optional(),
    message: z.string().optional(),
    source: source.optional(),
  })
  .loose()
/** Complete breakpoint response. */
export const dapBreakpoints = z.object({ breakpoints: z.array(dapBreakpoint) }).loose()
/** One breakpoint verification update. */
export const dapBreakpointEvent = z.object({ breakpoint: dapBreakpoint }).loose()
/** Stopped-event facts. */
export const dapStopped = z.object({ reason: z.string(), threadId: reference.optional() }).loose()
/** Exited-event facts. */
export const dapExited = z.object({ exitCode: z.number().int() }).loose()
/** Output-event facts. */
export const dapOutput = z.object({ output: z.string(), category: z.string().optional() }).loose()
/** Thread list. */
export const dapThreads = z.object({ threads: z.array(z.object({ id: reference, name: z.string() })) }).loose()
/** Stack trace response. */
export const dapStack = z
  .object({
    stackFrames: z.array(
      z.object({ id: reference, name: z.string(), line: reference, column: reference, source: source.optional() }).loose(),
    ),
  })
  .loose()
const children = { variablesReference: reference, namedVariables: reference.optional(), indexedVariables: reference.optional() }
/** Variable scope response. */
export const dapScopes = z.object({ scopes: z.array(z.object({ name: z.string(), expensive: z.boolean(), ...children })) }).loose()
/** Variable listing response. */
export const dapVariables = z
  .object({
    variables: z.array(
      z.object({ name: z.string(), value: z.string(), type: z.string().optional(), evaluateName: z.string().optional(), ...children }),
    ),
  })
  .loose()
/** Evaluation response. */
export const dapEvaluation = z.object({ result: z.string(), type: z.string().optional(), ...children }).loose()
/** Reverse terminal request; shell command strings and external terminals are unsupported. */
export const dapTerminal = z
  .object({
    kind: z.literal('integrated').optional(),
    cwd: z.string(),
    args: z.array(z.string().refine(value => !value.includes('\0'))).min(1),
    env: z.record(z.string(), z.string().nullable()).optional(),
    argsCanBeInterpretedByShell: z.literal(false).optional(),
  })
  .loose()
/** js-debug requests another connection for an opaque target from this owned adapter. */
export const dapStartDebugging = z
  .object({
    request: z.enum(['launch', 'attach']),
    configuration: z.object({ type: z.string(), name: z.string(), __pendingTargetId: z.string().min(1) }).loose(),
  })
  .loose()
