/** Launch-only Python, JavaScript, TypeScript and native debug sessions on the selected execution target. */
import { createConnection, createServer, type Server, type Socket } from 'node:net'
import { delimiter, win32 } from 'node:path'
import type { Readable, Writable } from 'node:stream'
import { StringDecoder } from 'node:string_decoder'
import { assertNever } from '../../shared/brand.ts'
import type {
  IdeDebugSnapshot,
  IdeExecutionRequest,
  IdeExecutionResponse,
  IdeSourceBreakpoints,
  IdeVerifiedBreakpoint,
} from '../../shared/ide-execution-protocol.ts'
import type { TerminalHandle } from '../process.ts'
import { IdeDapPeer } from './debug-protocol.ts'
import * as values from './debug-values.ts'
import { IdeProcessOwner, type IdeOutputStream, type IdeSubprocess } from './execution-process.ts'
import {
  absoluteIdePath,
  idePathApi,
  resolveIdeExecutable,
  resolveIdeWorkspacePath,
  type IdeRunResolverOptions,
  type ResolvedIdeRun,
} from './execution-resolve.ts'
import type { IdeExecutionLimits } from './execution-schema.ts'

type DebugRequest = Extract<IdeExecutionRequest, { readonly debugId: unknown }>

/** Dependencies and operation-owned publication callbacks. */
export interface IdeDebugOptions {
  readonly snapshot: IdeDebugSnapshot
  readonly resolved: ResolvedIdeRun
  readonly resolver: IdeRunResolverOptions
  readonly subprocess: IdeSubprocess
  readonly owner: IdeProcessOwner
  readonly limits: IdeExecutionLimits
  readonly cols: number
  readonly rows: number
  readonly stopOnEntry: boolean
  readonly breakpoints: readonly IdeSourceBreakpoints[]
  readonly publish: (snapshot: IdeDebugSnapshot) => void
  readonly output: (stream: IdeOutputStream, text: string) => void
  readonly reportError: (error: unknown) => void
}

function errorOf(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error))
}

/** All DAP transports and reverse-request terminals belong to one human-launched debuggee. */
export class IdeDebugSession {
  private readonly peers = new Set<IdeDapPeer>()
  private readonly terminatedPeers = new Set<IdeDapPeer>()
  private readonly sockets = new Set<Socket>()
  private readonly listeners = new Set<Server>()
  private readonly tasks = new Set<Promise<void>>()
  private readonly sources = new Map<string, readonly number[]>()
  private readonly variableCounts = new Map<number, number>()
  private active?: IdeDapPeer
  private rootPeer?: IdeDapPeer
  private terminal?: TerminalHandle
  private serverPort?: number
  private childClaimed = false
  private targetCount = 0
  private childInitialization?: Promise<void>
  private closing = false
  private stopped = false
  private stopGeneration = 0
  private cleanup?: Promise<void>
  private state: IdeDebugSnapshot
  private readonly complete: Promise<void>
  private completeResolve!: () => void
  /** Settles only when the adapter, launched process and terminal ranges have been joined. */
  get done(): Promise<void> {
    return this.complete
  }
  /** Latest immutable debug state. */
  get snapshot(): IdeDebugSnapshot {
    return this.state
  }

  /** @param options - resolved program, process owner and publication sinks. */
  constructor(private readonly options: IdeDebugOptions) {
    this.state = options.snapshot
    this.complete = new Promise<void>((resolve) => {
      this.completeResolve = resolve
    })
  }

  /**
   * Initialize an adapter after the controller has completed any native build.
   * @returns completed initialization; the session remains live until terminated.
   */
  async start(): Promise<void> {
    const deadline = setTimeout(() => {
      this.fail(new Error('The debug adapter did not finish startup before its deadline.'))
    }, this.options.limits.startupTimeoutMs)
    deadline.unref()
    try {
      this.update({ phase: 'initializing' })
      for (const source of this.options.breakpoints) {
        const path = await this.sourcePath(source.path)
        this.rememberSource(path, source.lines)
      }
      const peer = await this.createAdapter()
      this.rootPeer = peer
      this.active = peer
      await this.initialize(peer, 'launch', this.launchArguments())
      if (this.childInitialization) await this.childInitialization
      if (!this.closing && this.state.phase === 'initializing') this.update({ phase: 'running' })
    } catch (error) {
      if (!this.closing) this.fail(errorOf(error))
    } finally {
      clearTimeout(deadline)
    }
  }

  /**
   * Execute a whitelisted user debugger operation.
   * @param request - validated request scoped by the controller.
   * @returns adapter-confirmed response values.
   */
  async handle(request: DebugRequest): Promise<IdeExecutionResponse<DebugRequest>> {
    if (request.op === 'debug.stop') {
      await this.stop()
      return { ok: true }
    }
    if (this.closing) throw new Error('This debug session has ended.')
    if (request.op === 'debug.input') {
      if (!this.terminal) throw new Error('This debug session has no interactive terminal.')
      await this.terminal.write(request.data)
      return { ok: true }
    }
    if (request.op === 'debug.resize') {
      if (!this.terminal) throw new Error('This debug session has no interactive terminal.')
      await this.terminal.resize(request.cols, request.rows)
      return { ok: true }
    }
    const peer = this.active
    if (!peer) throw new Error('The debug adapter is still starting.')
    switch (request.op) {
      case 'debug.setBreakpoints': {
        if ((request.source.rootId ?? 'primary') !== (this.options.resolved.configuration.rootId ?? 'primary')) throw new Error('The breakpoint belongs to another project root.')
        const path = await this.sourcePath(request.source.path)
        this.rememberSource(path, request.source.lines)
        return this.setBreakpoints(peer, path, request.source.lines)
      }
      case 'debug.threads':
        return values.dapThreads.parse(await peer.request('threads', {})).threads
      case 'debug.stack':
        return values.dapStack
          .parse(
            await peer.request('stackTrace', { threadId: request.threadId, startFrame: 0, levels: this.options.limits.maxStackFrames }),
          )
          .stackFrames.slice(0, this.options.limits.maxStackFrames)
          .map(frame => ({
            id: frame.id,
            name: frame.name,
            line: frame.line,
            column: frame.column,
            ...(frame.source?.path ? { path: frame.source.path } : {}),
          }))
      case 'debug.scopes': {
        const scopes = values.dapScopes.parse(await peer.request('scopes', { frameId: request.frameId })).scopes
        for (const scope of scopes) this.rememberVariableCount(scope)
        return scopes
      }
      case 'debug.variables': {
        const start = request.start ?? 0
        const limit = Math.min(request.count ?? this.options.limits.maxVariables, this.options.limits.maxVariables)
        const total = this.variableCounts.get(request.variablesReference)
        const count = this.native && total !== undefined ? Math.min(limit, Math.max(0, total - start)) : limit
        if (count === 0) return []
        const unpaged = this.native && total === undefined
        const variables = values.dapVariables.parse(
          await peer.request('variables', { variablesReference: request.variablesReference, ...(unpaged ? {} : { start, count }) }),
        ).variables
        const page = unpaged ? variables.slice(start, start + limit) : variables.slice(0, limit)
        for (const variable of page) this.rememberVariableCount(variable)
        return page
      }
      case 'debug.evaluate': {
        if (this.native && request.context === 'repl')
          throw new Error('Native debugging accepts watch expressions; GDB command execution is unavailable.')
        return values.dapEvaluation.parse(
          await peer.request('evaluate', { expression: request.expression, frameId: request.frameId, context: request.context }),
        )
      }
      case 'debug.control': {
        const stoppedBefore = this.stopGeneration
        await peer.request(request.action, { threadId: request.threadId })
        if (this.native && request.action !== 'pause' && !this.isClosing() && stoppedBefore === this.stopGeneration) {
          this.variableCounts.clear()
          this.update({ phase: 'running', threadId: undefined, reason: undefined })
        }
        return { ok: true }
      }
      default:
        return assertNever(request)
    }
  }

  /** Stop this launched session and await managed process and transport cleanup. @returns quiescent completion. */
  async stop(): Promise<void> {
    if (!this.closing) {
      this.stopped = true
      this.update({ phase: 'stopping' })
    }
    await this.finish()
  }

  /** Record a controller-owned build or startup failure. @param error - failed build or preparation. */
  failStart(error: Error): void {
    this.fail(error)
  }

  private update(change: Partial<IdeDebugSnapshot>): void {
    let next = { ...this.state, ...change }
    if (next.phase === 'failed') {
      next = {
        ...next,
        error: next.error?.slice(0, Math.floor(this.options.limits.maxConfigurationBytes / 4)),
        reason: undefined,
        threadId: undefined,
      }
      if (Buffer.byteLength(JSON.stringify(next)) > this.options.limits.maxConfigurationBytes * 4) next = { ...next, breakpoints: [] }
    }
    if (Buffer.byteLength(JSON.stringify(next)) > this.options.limits.maxConfigurationBytes * 4)
      throw new Error('The debug session state exceeds the configured retention limit.')
    this.state = next
    this.options.publish(this.state)
  }

  private rememberSource(path: string, lines: readonly number[]): void {
    const next = new Map(this.sources)
    next.set(path, lines)
    if (Buffer.byteLength(JSON.stringify([...next])) > this.options.limits.maxConfigurationBytes)
      throw new Error('The debug breakpoints exceed the configured configuration limit.')
    this.sources.set(path, lines)
  }

  private rememberVariableCount(value: {
    variablesReference: number
    namedVariables?: number | undefined
    indexedVariables?: number | undefined
  }): void {
    if (value.variablesReference === 0 || (value.namedVariables === undefined && value.indexedVariables === undefined)) return
    this.variableCounts.set(value.variablesReference, (value.namedVariables ?? 0) + (value.indexedVariables ?? 0))
    if (this.variableCounts.size > this.options.limits.maxVariables * this.options.limits.maxStackFrames) {
      for (const reference of this.variableCounts.keys()) {
        this.variableCounts.delete(reference)
        break
      }
    }
  }

  private sourcePath(path: string): Promise<string> {
    return resolveIdeWorkspacePath(this.options.resolver.files, this.options.resolved.spec.workspaceRoot, path, 'file')
  }

  private track(task: Promise<void>): void {
    this.tasks.add(task)
    void task
      .catch((error: unknown) => {
        if (!this.closing) this.fail(errorOf(error))
      })
      .finally(() => {
        this.tasks.delete(task)
      })
  }

  private peer(input: Readable, output: Writable): IdeDapPeer {
    const peer = new IdeDapPeer({
      input,
      output,
      maxMessageBytes: this.options.limits.maxMessageBytes,
      requestTimeoutMs: this.options.limits.requestTimeoutMs,
      signal: this.options.owner.signal,
      onEvent: (event, body) => {
        this.event(peer, event, body)
      },
      onRequest: (command, body) => this.reverse(command, body),
      onFailure: (error) => {
        if (!this.closing && !this.terminatedPeers.has(peer)) this.fail(error)
      },
      reportCallbackError: this.options.reportError,
    })
    this.peers.add(peer)
    return peer
  }

  private async connect(): Promise<IdeDapPeer> {
    if (!this.serverPort || this.closing) throw new Error('The owned JavaScript debug server is unavailable.')
    const socket = createConnection({ host: '127.0.0.1', port: this.serverPort, signal: this.options.owner.signal })
    this.sockets.add(socket)
    await new Promise<void>((resolve, reject) => {
      const onError = (error: Error): void => {
        socket.off('connect', onConnect)
        reject(error)
      }
      const onConnect = (): void => {
        socket.off('error', onError)
        resolve()
      }
      socket.once('error', onError)
      socket.once('connect', onConnect)
    })
    if (this.isClosing()) {
      socket.destroy()
      throw new Error('Debugging was stopped.')
    }
    return this.peer(socket, socket)
  }

  private get native(): boolean {
    return this.options.resolved.spec.language === 'c' || this.options.resolved.spec.language === 'cpp'
  }

  /** A method read is not narrowed by an earlier check, unlike the field after an await. */
  private isClosing(): boolean {
    return this.closing
  }

  private async createAdapter(): Promise<IdeDapPeer> {
    const { resolved, owner, resolver } = this.options
    const { spec } = resolved
    const environment = { ...spec.launch.environment }
    const pathApi = idePathApi(spec.workspaceRoot)
    let argv: string[]
    if (spec.language === 'python') {
      environment.PYTHONPATH = [pathApi.join(resolver.resources.resourceRoot, 'python'), environment.PYTHONPATH].filter(Boolean).join(delimiter)
      environment.PYTHONDONTWRITEBYTECODE = '1'
      argv = [spec.launch.argv[0], '-u', '-m', 'debugpy.adapter']
    } else if (spec.language === 'javascript' || spec.language === 'typescript') {
      const adapter = pathApi.join(resolver.resources.resourceRoot, 'js-debug/src/dapDebugServer.js')
      if ((await resolver.files.kind(adapter)) !== 'file') throw new Error('The bundled JavaScript debug adapter is unavailable.')
      const process = owner.spawn(
        { argv: [absoluteIdePath(resolver.resources.node), adapter, '0', '127.0.0.1'], cwd: spec.launch.cwd, environment },
        'ignore',
      )
      let resolvePort!: (port: number) => void
      let rejectPort!: (error: Error) => void
      const ready = new Promise<number>((resolve, reject) => {
        resolvePort = resolve
        rejectPort = reject
      })
      let text = ''
      let listening = false
      const output = process.stdout
      if (!output) throw new Error('The JavaScript adapter has no output stream.')
      // A multi-byte character may span two chunks.
      const decoder = new StringDecoder('utf8')
      const read = async (): Promise<void> => {
        try {
          for await (const chunk of output) {
            const decoded = Buffer.isBuffer(chunk) ? decoder.write(chunk) : String(chunk)
            if (listening) {
              if (decoded) this.options.output('adapter', decoded)
              continue
            }
            text += decoded
            const match = /Debug server listening at 127\.0\.0\.1:(\d+)/.exec(text)
            if (match) {
              resolvePort(Number(match[1]))
              text = ''
              listening = true
            }
            if (Buffer.byteLength(text) > this.options.limits.maxEventBytes)
              throw new Error('The JavaScript adapter did not publish a valid listening address.')
          }
          rejectPort(new Error('The JavaScript adapter exited before publishing its listening address.'))
        } catch (error) {
          rejectPort(errorOf(error))
          throw error
        }
      }
      this.track(
        owner.waitProcess(process, [read(), owner.pump(process.stderr, 'adapter')]).then(() => {
          if (!this.closing) throw new Error('The JavaScript debug server exited unexpectedly.')
        }),
      )
      this.serverPort = await ready
      return this.connect()
    } else if (pathApi === win32) {
      const adapter = await resolveIdeExecutable(this.options.subprocess, undefined, 'codelldb.exe', environment)
      return this.nativeAdapter(adapter, environment)
    } else {
      const gdb = await resolveIdeExecutable(this.options.subprocess, undefined, 'gdb', environment)
      argv = [gdb, '--quiet', '--nx', '--interpreter=dap']
      if (spec.terminal) {
        const tty = await this.nativeTerminal()
        argv.push('-iex', `set inferior-tty ${tty}`)
      }
    }
    const process = owner.spawn({ argv, cwd: spec.launch.cwd, environment })
    if (!process.stdout || !process.stdin) throw new Error('The debug adapter has no DAP transport.')
    const peer = this.peer(process.stdout, process.stdin)
    this.track(
      owner.waitProcess(process, [owner.pump(process.stderr, 'adapter')]).then(() => {
        if (!this.closing) throw new Error('The debug adapter exited unexpectedly.')
      }),
    )
    return peer
  }

  private async nativeAdapter(adapter: string, environment: Record<string, string>): Promise<IdeDapPeer> {
    const { owner, resolved } = this.options
    const listener = createServer()
    this.listeners.add(listener)
    const connection = new Promise<Socket>((resolve, reject) => {
      const abort = (): void => { listener.close(); reject(new Error('Native debugging was stopped.')) }
      owner.signal.addEventListener('abort', abort, { once: true })
      listener.once('error', reject)
      listener.once('connection', (socket) => {
        owner.signal.removeEventListener('abort', abort)
        this.sockets.add(socket)
        listener.close()
        resolve(socket)
      })
      listener.once('close', () => {
        owner.signal.removeEventListener('abort', abort)
        reject(new Error('The native debug listener closed before the adapter connected.'))
      })
    })
    // Startup or owner cancellation observes the same rejection after listen/spawn.
    void connection.catch((_startupError: unknown) => { /* The awaited connection reports this startup failure. */ })
    await new Promise<void>((resolve, reject) => {
      listener.once('error', reject)
      listener.listen(0, '127.0.0.1', resolve)
    })
    const address = listener.address()
    if (!address || typeof address === 'string' || owner.signal.aborted) throw new Error('The native debug listener is unavailable.')
    const adapterProcess = owner.spawn({ argv: [adapter, '--connect', String(address.port)], cwd: resolved.spec.launch.cwd, environment }, 'ignore')
    this.track(owner.waitProcess(adapterProcess, [owner.pump(adapterProcess.stdout, 'adapter'), owner.pump(adapterProcess.stderr, 'adapter')]).then(() => {
      if (!this.closing) throw new Error('The native debug adapter exited unexpectedly.')
    }))
    const socket = await connection
    if (this.closing) { socket.destroy(); throw new Error('Native debugging was stopped.') }
    return this.peer(socket, socket)
  }

  private launchArguments(): Record<string, unknown> {
    const { spec, configuration } = this.options.resolved
    const common = {
      name: spec.name,
      program: spec.launch.argv[0],
      cwd: spec.launch.cwd,
      args: configuration.arguments ?? [],
      env: spec.launch.environment,
      stopOnEntry: this.options.stopOnEntry,
    }
    if (spec.language === 'python')
      return {
        ...common,
        type: 'python',
        program: configuration.pythonModule ? undefined : spec.program,
        module: configuration.pythonModule,
        python: [spec.launch.argv[0]],
        console: spec.terminal ? 'integratedTerminal' : 'internalConsole',
        redirectOutput: !spec.terminal,
        justMyCode: true,
        subProcess: false,
      }
    if (spec.language === 'javascript' || spec.language === 'typescript')
      return {
        ...common,
        type: 'pwa-node',
        program: spec.program,
        runtimeExecutable: spec.launch.argv[0],
        runtimeArgs: spec.launch.argv.slice(1, spec.launch.argv.indexOf(spec.program)),
        console: spec.terminal ? 'integratedTerminal' : 'internalConsole',
        sourceMaps: true,
        autoAttachChildProcesses: false,
        outputCapture: 'std',
      }
    return idePathApi(spec.workspaceRoot) === win32 ? { ...common, terminal: spec.terminal ? 'integrated' : 'console' }
      : { ...common, stopOnEntry: false, stopAtBeginningOfMainSubprogram: this.options.stopOnEntry }
  }

  private async initialize(peer: IdeDapPeer, command: 'launch' | 'attach', arguments_: Record<string, unknown>): Promise<void> {
    const deadline = setTimeout(() => {
      this.fail(new Error('A debug adapter connection did not finish initialization before its deadline.'))
    }, this.options.limits.startupTimeoutMs)
    deadline.unref()
    try {
      const capabilities = values.dapCapabilities.parse(
        await peer.request('initialize', {
          clientID: 'rainy-ide',
          clientName: 'RainyAgent IDE',
          adapterID: this.options.resolved.spec.language,
          pathFormat: 'path',
          linesStartAt1: true,
          columnsStartAt1: true,
          supportsRunInTerminalRequest: true,
          supportsStartDebuggingRequest: true,
          supportsVariablePaging: true,
          supportsVariableType: true,
          locale: 'en-US',
        }),
      )
      if (peer === this.active)
        this.update({
          capabilities: {
            configurationDone: capabilities.supportsConfigurationDoneRequest === true,
            evaluate: true,
            pause: true,
            stepIn: true,
            stepOut: true,
            next: true,
          },
        })
      const ownedJavaScriptTarget = this.serverPort !== undefined && typeof arguments_.__pendingTargetId === 'string'
      const receivesBreakpoints = peer === this.active || peer === this.rootPeer
      if (ownedJavaScriptTarget) await peer.initialized
      const bootstrapSequence = peer.initializationSequence
      const launch = peer.request(command, arguments_)
      void launch.catch(() => {
        /* Initialization below observes launch failures. */
      })
      await Promise.race([peer.initialized, launch.then(() => peer.initialized)])
      if (receivesBreakpoints) for (const [path, lines] of this.sources) await this.setBreakpoints(peer, path, lines)
      if (capabilities.supportsConfigurationDoneRequest) await peer.request('configurationDone', {})
      if (ownedJavaScriptTarget) await peer.initializedAfter(bootstrapSequence)
      await launch
      if (ownedJavaScriptTarget && receivesBreakpoints)
        for (const [path, lines] of this.sources) await this.setBreakpoints(peer, path, lines)
    } finally {
      clearTimeout(deadline)
    }
  }

  private async setBreakpoints(peer: IdeDapPeer, path: string, lines: readonly number[]): Promise<readonly IdeVerifiedBreakpoint[]> {
    const result = values.dapBreakpoints.parse(
      await peer.request('setBreakpoints', {
        source: { name: idePathApi(path).basename(path), path },
        breakpoints: lines.map(line => ({ line })),
        sourceModified: false,
      }),
    )
    if (result.breakpoints.length > lines.length) throw new Error('The adapter returned more breakpoints than requested.')
    // js-debug returns an empty list for an unchanged set; later events verify the existing IDs.
    if (this.serverPort && result.breakpoints.length === 0 && lines.length > 0) {
      const unchanged = this.state.breakpoints.filter(
        breakpoint => breakpoint.path === path && breakpoint.requestedLine !== undefined && lines.includes(breakpoint.requestedLine),
      )
      if (unchanged.length === lines.length) return unchanged
    }
    const breakpoints = result.breakpoints.map((breakpoint, index) => ({
      path,
      requestedLine: lines[index],
      verified: breakpoint.verified,
      id: breakpoint.id,
      line: breakpoint.line,
      message: breakpoint.message,
    }))
    if (!this.closing && peer === this.active)
      this.update({ breakpoints: [...this.state.breakpoints.filter(breakpoint => breakpoint.path !== path), ...breakpoints] })
    return breakpoints
  }

  private async reverse(command: string, body: unknown): Promise<unknown> {
    if (this.closing) throw new Error('Debugging was stopped.')
    if (command === 'runInTerminal') {
      const request = values.dapTerminal.parse(body)
      if (this.terminal) throw new Error('This debug session already owns an interactive terminal.')
      const cwd = await resolveIdeWorkspacePath(
        this.options.resolver.files,
        this.options.resolved.spec.workspaceRoot,
        request.cwd,
        'directory',
      )
      const environment: Record<string, string> = {}
      for (const [name, value] of Object.entries(request.env ?? {})) {
        if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name) || /^DSH_/i.test(name))
          throw new Error('The adapter requested a reserved terminal environment variable.')
        environment[name] = value ?? ''
      }
      const executable = await resolveIdeExecutable(this.options.subprocess, request.args[0], request.args[0], environment)
      const terminal = await this.options.owner.terminal(
        { argv: [executable, ...request.args.slice(1)], cwd, environment },
        this.options.cols,
        this.options.rows,
      )
      this.terminal = terminal
      this.track(
        this.options.owner.waitTerminal(terminal, this.options.owner.pump(terminal.output, 'terminal')).then((outcome) => {
          if (!this.state.exit) this.update({ exit: { exitCode: outcome.exitCode, signal: outcome.signal, stopped: this.stopped } })
        }),
      )
      return { processId: terminal.pid, shellProcessId: terminal.pid }
    }
    if (command === 'startDebugging' && this.serverPort) {
      const request = values.dapStartDebugging.parse(body)
      if (this.targetCount >= this.options.limits.maxDebugTargets) {
        const error = new Error('The launched JavaScript program exceeds the configured debug target limit.')
        this.fail(error)
        throw error
      }
      this.targetCount++
      const mainTarget = !this.childClaimed
      if (mainTarget) this.childClaimed = true
      const task = (async () => {
        const peer = await this.connect()
        if (mainTarget) this.active = peer
        await this.initialize(peer, request.request, {
          type: request.configuration.type,
          name: request.configuration.name,
          __pendingTargetId: request.configuration.__pendingTargetId,
        })
      })()
      if (mainTarget) this.childInitialization = task
      this.track(task)
      return {}
    }
    throw new Error(`Unsupported debug adapter request: ${command}`)
  }

  private event(peer: IdeDapPeer, event: string, body: unknown): void {
    if (this.closing) return
    if (event === 'output') {
      const result = values.dapOutput.parse(body)
      if (result.category !== 'telemetry')
        this.options.output(result.category === 'stderr' ? 'stderr' : result.category === 'stdout' ? 'stdout' : 'adapter', result.output)
      return
    }
    if (event === 'exited') {
      const value = values.dapExited.parse(body)
      if (peer === this.rootPeer || peer === this.active)
        this.update({ exit: { exitCode: value.exitCode, signal: null, stopped: this.stopped } })
      return
    }
    if (event === 'terminated') {
      this.terminatedPeers.add(peer)
      if (!this.serverPort || peer === this.rootPeer) void this.finish().catch(this.options.reportError)
      return
    }
    if (peer !== this.active) return
    switch (event) {
      case 'stopped': {
        const value = values.dapStopped.parse(body)
        this.stopGeneration++
        this.variableCounts.clear()
        this.update({ phase: 'paused', reason: value.reason, threadId: value.threadId })
        return
      }
      case 'continued':
        this.variableCounts.clear()
        this.update({ phase: 'running', threadId: undefined, reason: undefined })
        return
      case 'breakpoint': {
        const value = values.dapBreakpointEvent.parse(body).breakpoint
        const prior = this.state.breakpoints.find(breakpoint => value.id !== undefined && breakpoint.id === value.id)
        if (prior)
          this.update({
            breakpoints: this.state.breakpoints.map(breakpoint =>
              breakpoint === prior ? { ...prior, verified: value.verified, line: value.line, message: value.message } : breakpoint,
            ),
          })
        return
      }
      default:
        return // Adapter-specific events do not change the supported debugger state.
    }
  }

  private async nativeTerminal(): Promise<string> {
    const { owner, subprocess, resolved, limits } = this.options
    const python = await resolveIdeExecutable(subprocess, undefined, 'python3', {})
    const script =
      'import os,signal\nsignal.signal(signal.SIGINT,signal.SIG_IGN)\nprint(os.ttyname(0),flush=True)\nwhile True: signal.pause()'
    const terminal = await owner.terminal(
      { argv: [python, '-u', '-c', script], cwd: resolved.spec.launch.cwd, environment: {} },
      this.options.cols,
      this.options.rows,
    )
    this.terminal = terminal
    let resolveTty!: (tty: string) => void
    let rejectTty!: (error: Error) => void
    const ready = new Promise<string>((resolve, reject) => {
      resolveTty = resolve
      rejectTty = reject
    })
    const pump = (async () => {
      let first = true
      let prefix = ''
      const decoder = new StringDecoder('utf8')
      try {
        for await (const chunk of terminal.output) {
          if (!Buffer.isBuffer(chunk) && typeof chunk !== 'string') throw new Error('The debug terminal returned a non-byte stream.')
          const text = decoder.write(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk))
          if (!first) {
            this.options.output('terminal', text)
            continue
          }
          prefix += text
          const newline = prefix.indexOf('\n')
          if (newline < 0) {
            if (prefix.length > limits.maxEventBytes) throw new Error('The debug terminal did not report its device.')
            continue
          }
          const tty = prefix.slice(0, newline).trim()
          if (!/^\/dev\/pts\/\d+$/.test(tty)) throw new Error('The debug terminal returned an invalid device.')
          first = false
          resolveTty(tty)
          if (prefix.slice(newline + 1)) this.options.output('terminal', prefix.slice(newline + 1))
        }
        const tail = decoder.end()
        if (!first && tail) this.options.output('terminal', tail)
        if (first) rejectTty(new Error('The debug terminal closed before reporting its device.'))
      } catch (error) {
        rejectTty(errorOf(error))
        throw error
      }
    })()
    this.track(owner.waitTerminal(terminal, pump).then(() => {}))
    return ready
  }

  private fail(error: Error): void {
    if (this.closing) return
    this.update({ phase: 'failed', error: error.message.slice(0, Math.floor(this.options.limits.maxConfigurationBytes / 4)) })
    void this.finish().catch(this.options.reportError)
  }

  private finish(): Promise<void> {
    if (this.cleanup) return this.cleanup
    this.closing = true
    for (const listener of this.listeners) listener.close()
    const peers = [...this.peers].map(peer => peer.close())
    const sockets = [...this.sockets].map(
      socket =>
        new Promise<void>((resolve) => {
          if (socket.closed) {
            resolve()
            return
          }
          socket.once('close', () => {
            resolve()
          })
          socket.destroy()
        }),
    )
    this.cleanup = (async () => {
      try {
        await this.options.owner.close()
        await Promise.all([...peers, ...sockets])
        await Promise.allSettled([...this.tasks])
        if (this.state.phase !== 'failed')
          this.update({
            phase: 'terminated',
            threadId: undefined,
            reason: undefined,
            exit: { exitCode: this.state.exit?.exitCode ?? null, signal: this.state.exit?.signal ?? null, stopped: this.stopped },
          })
      } catch (error) {
        this.update({ phase: 'failed', error: errorOf(error).message })
        throw error
      } finally {
        this.completeResolve()
      }
    })()
    return this.cleanup
  }
}
