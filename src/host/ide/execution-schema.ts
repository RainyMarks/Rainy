/** JSON admission for human run/debug requests and durable workspace execution settings. */
import { z } from 'zod'
import { brandString } from '@deepseek-ai/dsh-brand'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type { IdeRootId } from '@deepseek-ai/dsh-client-ui-rainy/ide-files-protocol'
import type {
  IdeDebugId,
  IdeExecutionConfiguration,
  IdeExecutionRequest,
  IdeRunConfiguration,
  IdeRunId,
  IdeTerminalId,
} from '@deepseek-ai/dsh-client-ui-rainy/ide-execution-protocol'

const text = z.string().refine(value => !value.includes('\0'), 'NUL characters are not accepted.')
const path = text.min(1)
const line = z.number().int().positive()
const reference = z.number().int().nonnegative()
const workspaceId = text.min(1).transform(WorkspaceId)
const rootId = text.min(1).transform(value => brandString<IdeRootId>(value))
const runId = z.uuid().transform(value => brandString<IdeRunId>(value))
const terminalId = z.uuid().transform(value => brandString<IdeTerminalId>(value))
const debugId = z.uuid().transform(value => brandString<IdeDebugId>(value))
const environment = z.record(
  z
    .string()
    .regex(/^[A-Za-z_][A-Za-z0-9_]*$/)
    .refine(value => !/^DSH_/i.test(value), 'DSH environment names are reserved.'),
  text,
)
const build = z.discriminatedUnion('kind', [
  z.object({ kind: z.literal('single-file'), flags: z.array(text).optional() }).strict(),
  z
    .object({
      kind: z.literal('cmake'),
      buildDirectory: path,
      target: path,
      executable: path,
      configurePreset: path.optional(),
      buildPreset: path.optional(),
    })
    .strict(),
])

/** Validated persisted run choices; filesystem and executable checks belong to resolve(). */
export const ideRunConfigurationSchema: z.ZodType<IdeRunConfiguration> = z
  .object({
    name: text.min(1),
    language: z.enum(['python', 'javascript', 'typescript', 'php', 'c', 'cpp']),
    rootId: rootId.optional(),
    program: path,
    pythonModule: text.regex(/^[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*$/, 'Use a dotted Python module name.').optional(),
    cwd: path.optional(),
    arguments: z.array(text).optional(),
    environment: environment.optional(),
    executable: path.optional(),
    build: build.optional(),
    terminal: z.boolean().optional(),
  })
  .strict()
  .refine(value => !value.build || value.language === 'c' || value.language === 'cpp', 'Build settings require C or C++.')
  .refine(value => !value.pythonModule || value.language === 'python', 'Python module settings require Python.')

/** Complete breakpoint replacement for one file. */
export const ideSourceBreakpointsSchema = z.object({ rootId: rootId.optional(), path, lines: z.array(line) }).strict()

/** Durable configuration schema imported by workspace state; activeProfile refers to a profile name. */
export const ideExecutionConfigurationSchema: z.ZodType<IdeExecutionConfiguration> = z
  .object({
    profiles: z.array(ideRunConfigurationSchema),
    activeProfile: text.nullable(),
    breakpoints: z.array(ideSourceBreakpointsSchema),
    watches: z.array(text),
  })
  .strict()
  .superRefine((value, context) => {
    const names = value.profiles.map(profile => profile.name)
    if (new Set(names).size !== names.length)
      context.addIssue({ code: 'custom', message: 'Run profile names must be unique.', path: ['profiles'] })
    if (value.activeProfile !== null && !names.includes(value.activeProfile))
      context.addIssue({ code: 'custom', message: 'The selected run profile does not exist.', path: ['activeProfile'] })
  })

const dimension = z.number().int().positive()
const configuration = ideRunConfigurationSchema
const scope = { workspaceId }
const debugScope = { ...scope, debugId }
const terminalScope = { ...scope, terminalId }
const runScope = { ...scope, runId }

/** Deployment-configurable execution retention, protocol, terminal, and lifecycle bounds. */
export const ideExecutionLimitsSchema = z
  .object({
    maxOutputBytes: z
      .number()
      .int()
      .min(4096)
      .default(8 * 1024 * 1024),
    maxEventBytes: z
      .number()
      .int()
      .min(1024)
      .default(256 * 1024),
    maxOperations: z.number().int().min(1).default(16),
    maxDebugTargets: z.number().int().min(1).default(16),
    maxConfigurationBytes: z
      .number()
      .int()
      .min(256)
      .default(32 * 1024),
    maxMessageBytes: z
      .number()
      .int()
      .min(1024)
      .default(8 * 1024 * 1024),
    requestTimeoutMs: z.number().int().positive().max(2147483647).default(15000),
    startupTimeoutMs: z.number().int().positive().max(2147483647).default(60000),
    killGraceMs: z.number().int().positive().max(2147483647).default(1500),
    defaultCols: z.number().int().positive().default(100),
    defaultRows: z.number().int().positive().default(24),
    maxTerminalDimension: z.number().int().positive().default(10000),
    maxStackFrames: z.number().int().positive().default(100),
    maxVariables: z.number().int().positive().default(200),
  })
  .strict()
  .superRefine((value, context) => {
    if (value.maxEventBytes > value.maxOutputBytes / 2)
      context.addIssue({ code: 'custom', message: 'maxOutputBytes must hold at least two complete events.' })
    if (value.maxConfigurationBytes * value.maxOperations * 4 > value.maxOutputBytes)
      context.addIssue({ code: 'custom', message: 'Output retention must cover all retained configuration summaries.' })
    if (value.defaultCols > value.maxTerminalDimension || value.defaultRows > value.maxTerminalDimension)
      context.addIssue({ code: 'custom', message: 'Default terminal dimensions exceed maxTerminalDimension.' })
  })

/** Fully resolved execution limits passed to the controller. */
export type IdeExecutionLimits = z.output<typeof ideExecutionLimitsSchema>

/** Whitelisted operations accepted from the authenticated product endpoint. */
export const ideExecutionRequestSchema: z.ZodType<IdeExecutionRequest> = z.discriminatedUnion('op', [
  z.object({ op: z.literal('execution.status'), ...scope }).strict(),
  z.object({ op: z.literal('execution.poll'), ...scope, cursor: reference }).strict(),
  z.object({ op: z.literal('terminal.start'), ...scope, rootId: rootId.optional(), cwd: path.optional(), cols: dimension, rows: dimension }).strict(),
  z.object({ op: z.literal('terminal.input'), ...terminalScope, data: z.string() }).strict(),
  z.object({ op: z.literal('terminal.resize'), ...terminalScope, cols: dimension, rows: dimension }).strict(),
  z.object({ op: z.literal('terminal.stop'), ...terminalScope }).strict(),
  z.object({ op: z.literal('run.resolve'), ...scope, configuration }).strict(),
  z.object({ op: z.literal('run.start'), ...scope, configuration, cols: dimension.optional(), rows: dimension.optional() }).strict(),
  z.object({ op: z.literal('run.input'), ...runScope, data: z.string() }).strict(),
  z.object({ op: z.literal('run.resize'), ...runScope, cols: dimension, rows: dimension }).strict(),
  z.object({ op: z.literal('run.stop'), ...runScope }).strict(),
  z
    .object({
      op: z.literal('debug.start'),
      ...scope,
      configuration,
      breakpoints: z.array(ideSourceBreakpointsSchema),
      stopOnEntry: z.boolean().optional(),
      cols: dimension.optional(),
      rows: dimension.optional(),
    })
    .strict(),
  z.object({ op: z.literal('debug.setBreakpoints'), ...debugScope, source: ideSourceBreakpointsSchema }).strict(),
  z.object({ op: z.literal('debug.threads'), ...debugScope }).strict(),
  z.object({ op: z.literal('debug.stack'), ...debugScope, threadId: reference }).strict(),
  z.object({ op: z.literal('debug.scopes'), ...debugScope, frameId: reference }).strict(),
  z
    .object({
      op: z.literal('debug.variables'),
      ...debugScope,
      variablesReference: reference,
      start: reference.optional(),
      count: dimension.optional(),
    })
    .strict(),
  z
    .object({
      op: z.literal('debug.evaluate'),
      ...debugScope,
      expression: text.min(1),
      frameId: reference.optional(),
      context: z.enum(['watch', 'repl']),
    })
    .strict(),
  z
    .object({
      op: z.literal('debug.control'),
      ...debugScope,
      action: z.enum(['continue', 'pause', 'next', 'stepIn', 'stepOut']),
      threadId: reference,
    })
    .strict(),
  z.object({ op: z.literal('debug.input'), ...debugScope, data: z.string() }).strict(),
  z.object({ op: z.literal('debug.resize'), ...debugScope, cols: dimension, rows: dimension }).strict(),
  z.object({ op: z.literal('debug.stop'), ...debugScope }).strict(),
])
