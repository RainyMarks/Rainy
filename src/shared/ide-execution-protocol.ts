/** Human-operated WSL execution and launch-only debugging messages for POST /rainy/ide. */
import type { Branded } from './brand.ts'
import type { WorkspaceId } from './ide-files-protocol.ts'
import type { IdeRootId } from './ide-files-protocol.ts'

/** One independently owned program execution. */
export type IdeRunId = Branded<'RainyIdeRunId'>
/** One independently owned interactive terminal. */
export type IdeTerminalId = Branded<'RainyIdeTerminalId'>
/** One independently owned launch-only debug session. */
export type IdeDebugId = Branded<'RainyIdeDebugId'>
/** An operation whose output appears in the workspace event stream. */
export type IdeOperationId = IdeRunId | IdeTerminalId | IdeDebugId
/** Languages with prepared execution and debugging support. */
export type IdeExecutionLanguage = 'python' | 'javascript' | 'typescript' | 'php' | 'c' | 'cpp'

/** C/C++ compilation choices; all paths are relative to the selected workspace. */
export type IdeBuildConfiguration =
  | { readonly kind: 'single-file'; readonly flags?: readonly string[] | undefined }
  | {
    readonly kind: 'cmake'
    readonly buildDirectory: string
    readonly target: string
    readonly executable: string
    readonly configurePreset?: string | undefined
    readonly buildPreset?: string | undefined
  }

/** A named human launch configuration; an explicit interpreter/compiler must be an absolute WSL path. */
export interface IdeRunConfiguration {
  /** Root containing the program; omission selects the primary root. */
  readonly rootId?: IdeRootId | undefined
  readonly name: string
  readonly language: IdeExecutionLanguage
  readonly program: string
  /** Python module entrypoint; absent runs the selected file. */
  readonly pythonModule?: string | undefined
  readonly cwd?: string | undefined
  readonly arguments?: readonly string[] | undefined
  readonly environment?: Readonly<Record<string, string>> | undefined
  readonly executable?: string | undefined
  readonly build?: IdeBuildConfiguration | undefined
  readonly terminal?: boolean | undefined
}

/** Complete line-breakpoint replacement for one workspace source file. */
export interface IdeSourceBreakpoints {
  readonly rootId?: IdeRootId | undefined
  readonly path: string
  readonly lines: readonly number[]
}

/** Persistable workspace settings; operation identities and runtime output are intentionally absent. */
export interface IdeExecutionConfiguration {
  readonly profiles: readonly IdeRunConfiguration[]
  readonly activeProfile: string | null
  readonly breakpoints: readonly IdeSourceBreakpoints[]
  readonly watches: readonly string[]
}

/** Fully specified command in the WSL filesystem; argv is never interpreted by a shell. */
export interface IdeCommandSpec {
  readonly argv: readonly string[]
  readonly cwd: string
  readonly environment: Readonly<Record<string, string>>
}

/** Resolved execution preview. Resolving does not start a process or reserve an operation. */
export interface IdeResolvedRunSpec {
  readonly rootId?: IdeRootId | undefined
  readonly workspaceId: WorkspaceId
  readonly workspaceRoot: string
  readonly language: IdeExecutionLanguage
  readonly program: string
  readonly name: string
  readonly launch: IdeCommandSpec
  readonly build: readonly IdeCommandSpec[]
  readonly buildDirectory?: string | undefined
  readonly terminal: boolean
}

/** Independent completion facts; a requested stop can coincide with a zero exit code. */
export interface IdeProcessExit {
  readonly exitCode: number | null
  readonly signal: string | null
  readonly stopped: boolean
}

/** Current state of one run. */
export interface IdeRunSnapshot {
  readonly id: IdeRunId
  readonly workspaceId: WorkspaceId
  readonly name: string
  readonly phase: 'starting' | 'building' | 'running' | 'stopping' | 'exited' | 'failed'
  readonly spec: IdeResolvedRunSpec
  readonly exit?: IdeProcessExit | undefined
  readonly error?: string | undefined
}

/** Current state of one interactive terminal. */
export interface IdeTerminalSnapshot {
  readonly id: IdeTerminalId
  readonly workspaceId: WorkspaceId
  readonly cwd: string
  readonly phase: 'starting' | 'running' | 'stopping' | 'exited' | 'failed'
  readonly exit?: IdeProcessExit | undefined
  readonly error?: string | undefined
}

/** Adapter-confirmed line breakpoint; verification can change after launch. */
export interface IdeVerifiedBreakpoint {
  readonly id?: number | undefined
  readonly path: string
  readonly requestedLine?: number | undefined
  readonly line?: number | undefined
  readonly verified: boolean
  readonly message?: string | undefined
}

/** One debuggee thread reported by the adapter. */
export interface IdeDebugThread {
  readonly id: number
  readonly name: string
}
/** One stack frame; paths may name runtime sources outside the editable workspace. */
export interface IdeDebugFrame {
  readonly id: number
  readonly name: string
  readonly path?: string | undefined
  readonly line: number
  readonly column: number
}
/** One expandable variable scope in a selected frame. */
export interface IdeDebugScope {
  readonly name: string
  readonly variablesReference: number
  readonly expensive: boolean
  readonly namedVariables?: number | undefined
  readonly indexedVariables?: number | undefined
}
/** One displayed variable or watch result; child values are fetched separately. */
export interface IdeDebugVariable {
  readonly name: string
  readonly value: string
  readonly type?: string | undefined
  readonly variablesReference: number
  readonly namedVariables?: number | undefined
  readonly indexedVariables?: number | undefined
  readonly evaluateName?: string | undefined
}
/** Result of evaluating an explicit user expression in the launched debuggee. */
export interface IdeDebugEvaluation {
  readonly result: string
  readonly type?: string | undefined
  readonly variablesReference: number
  readonly namedVariables?: number | undefined
  readonly indexedVariables?: number | undefined
}

/** Debug controls supported by this client and an initialized adapter. */
export interface IdeDebugCapabilities {
  readonly configurationDone: boolean
  readonly evaluate: boolean
  readonly pause: boolean
  readonly stepIn: boolean
  readonly stepOut: boolean
  readonly next: boolean
}

/** Current state of one launch-only debug session. */
export interface IdeDebugSnapshot {
  readonly rootId?: IdeRootId | undefined
  readonly id: IdeDebugId
  readonly workspaceId: WorkspaceId
  readonly name: string
  readonly language: IdeExecutionLanguage
  readonly phase: 'starting' | 'building' | 'initializing' | 'running' | 'paused' | 'stopping' | 'terminated' | 'failed'
  readonly threadId?: number | undefined
  readonly reason?: string | undefined
  readonly capabilities?: IdeDebugCapabilities | undefined
  readonly breakpoints: readonly IdeVerifiedBreakpoint[]
  readonly exit?: IdeProcessExit | undefined
  readonly error?: string | undefined
}

/** Latest workspace state accompanies every poll so retained history cannot roll it backwards. */
export interface IdeExecutionStatus {
  readonly runs: readonly IdeRunSnapshot[]
  readonly terminals: readonly IdeTerminalSnapshot[]
  readonly debugSessions: readonly IdeDebugSnapshot[]
}

/** Ordered workspace event. Output retention is bounded and lost history is reported by poll.truncated. */
export type IdeExecutionEvent = { readonly sequence: number; readonly workspaceId: WorkspaceId } & (
  | {
    readonly kind: 'output'
    readonly operationId: IdeOperationId
    readonly stream: 'stdout' | 'stderr' | 'terminal' | 'adapter'
    readonly text: string
  }
  | { readonly kind: 'run'; readonly run: IdeRunSnapshot }
  | { readonly kind: 'terminal'; readonly terminal: IdeTerminalSnapshot }
  | { readonly kind: 'debug'; readonly debug: IdeDebugSnapshot }
  | { readonly kind: 'breakpoints'; readonly debugId: IdeDebugId; readonly breakpoints: readonly IdeVerifiedBreakpoint[] }
)

/** Cursor advances over immutable events; status is current at response creation. */
export interface IdeExecutionPoll {
  readonly events: readonly IdeExecutionEvent[]
  readonly cursor: number
  readonly truncated: boolean
  readonly status: IdeExecutionStatus
}

/** Browser requests accepted by the execution handler; there is no attach or raw DAP operation. */
export type IdeExecutionRequest =
  | { readonly op: 'execution.status'; readonly workspaceId: WorkspaceId }
  | { readonly op: 'execution.poll'; readonly workspaceId: WorkspaceId; readonly cursor: number }
  | {
    readonly op: 'terminal.start'
    readonly workspaceId: WorkspaceId
    readonly rootId?: IdeRootId | undefined
    readonly cwd?: string | undefined
    readonly cols: number
    readonly rows: number
  }
  | { readonly op: 'terminal.input'; readonly workspaceId: WorkspaceId; readonly terminalId: IdeTerminalId; readonly data: string }
  | {
    readonly op: 'terminal.resize'
    readonly workspaceId: WorkspaceId
    readonly terminalId: IdeTerminalId
    readonly cols: number
    readonly rows: number
  }
  | { readonly op: 'terminal.stop'; readonly workspaceId: WorkspaceId; readonly terminalId: IdeTerminalId }
  | { readonly op: 'run.resolve'; readonly workspaceId: WorkspaceId; readonly configuration: IdeRunConfiguration }
  | {
    readonly op: 'run.start'
    readonly workspaceId: WorkspaceId
    readonly configuration: IdeRunConfiguration
    readonly cols?: number | undefined
    readonly rows?: number | undefined
  }
  | { readonly op: 'run.input'; readonly workspaceId: WorkspaceId; readonly runId: IdeRunId; readonly data: string }
  | { readonly op: 'run.resize'; readonly workspaceId: WorkspaceId; readonly runId: IdeRunId; readonly cols: number; readonly rows: number }
  | { readonly op: 'run.stop'; readonly workspaceId: WorkspaceId; readonly runId: IdeRunId }
  | {
    readonly op: 'debug.start'
    readonly workspaceId: WorkspaceId
    readonly configuration: IdeRunConfiguration
    readonly breakpoints: readonly IdeSourceBreakpoints[]
    readonly stopOnEntry?: boolean | undefined
    readonly cols?: number | undefined
    readonly rows?: number | undefined
  }
  | {
    readonly op: 'debug.setBreakpoints'
    readonly workspaceId: WorkspaceId
    readonly debugId: IdeDebugId
    readonly source: IdeSourceBreakpoints
  }
  | { readonly op: 'debug.threads'; readonly workspaceId: WorkspaceId; readonly debugId: IdeDebugId }
  | { readonly op: 'debug.stack'; readonly workspaceId: WorkspaceId; readonly debugId: IdeDebugId; readonly threadId: number }
  | { readonly op: 'debug.scopes'; readonly workspaceId: WorkspaceId; readonly debugId: IdeDebugId; readonly frameId: number }
  | {
    readonly op: 'debug.variables'
    readonly workspaceId: WorkspaceId
    readonly debugId: IdeDebugId
    readonly variablesReference: number
    readonly start?: number | undefined
    readonly count?: number | undefined
  }
  | {
    readonly op: 'debug.evaluate'
    readonly workspaceId: WorkspaceId
    readonly debugId: IdeDebugId
    readonly expression: string
    readonly frameId?: number | undefined
    readonly context: 'watch' | 'repl'
  }
  | {
    readonly op: 'debug.control'
    readonly workspaceId: WorkspaceId
    readonly debugId: IdeDebugId
    readonly action: 'continue' | 'pause' | 'next' | 'stepIn' | 'stepOut'
    readonly threadId: number
  }
  | { readonly op: 'debug.input'; readonly workspaceId: WorkspaceId; readonly debugId: IdeDebugId; readonly data: string }
  | {
    readonly op: 'debug.resize'
    readonly workspaceId: WorkspaceId
    readonly debugId: IdeDebugId
    readonly cols: number
    readonly rows: number
  }
  | { readonly op: 'debug.stop'; readonly workspaceId: WorkspaceId; readonly debugId: IdeDebugId }

/** Response value for each operation; the HTTP route adds its common ok/value envelope. */
export interface IdeExecutionResponseMap {
  readonly 'execution.status': IdeExecutionStatus
  readonly 'execution.poll': IdeExecutionPoll
  readonly 'terminal.start': IdeTerminalSnapshot
  readonly 'terminal.input': { readonly ok: true }
  readonly 'terminal.resize': { readonly ok: true }
  readonly 'terminal.stop': { readonly ok: true }
  readonly 'run.resolve': IdeResolvedRunSpec
  readonly 'run.start': IdeRunSnapshot
  readonly 'run.input': { readonly ok: true }
  readonly 'run.resize': { readonly ok: true }
  readonly 'run.stop': { readonly ok: true }
  readonly 'debug.start': IdeDebugSnapshot
  readonly 'debug.setBreakpoints': readonly IdeVerifiedBreakpoint[]
  readonly 'debug.threads': readonly IdeDebugThread[]
  readonly 'debug.stack': readonly IdeDebugFrame[]
  readonly 'debug.scopes': readonly IdeDebugScope[]
  readonly 'debug.variables': readonly IdeDebugVariable[]
  readonly 'debug.evaluate': IdeDebugEvaluation
  readonly 'debug.control': { readonly ok: true }
  readonly 'debug.input': { readonly ok: true }
  readonly 'debug.resize': { readonly ok: true }
  readonly 'debug.stop': { readonly ok: true }
}

/** Typed result projection for one admitted request. */
export type IdeExecutionResponse<T extends IdeExecutionRequest = IdeExecutionRequest> = IdeExecutionResponseMap[T['op']]
