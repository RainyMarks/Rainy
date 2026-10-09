/** Controlled lifecycle, argv, containment and bounded-history evidence for the human execution service. */
import { PassThrough } from 'node:stream'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type {
  SubprocessHandle,
  SubprocessOutcome,
  SubprocessSpawnSpec,
  SubprocessTerminalHandle,
  SubprocessTerminalSpawnSpec,
} from '@deepseek-ai/dsh-subprocess'
import type {
  IdeExecutionPoll,
  IdeExecutionRequest,
  IdeExecutionResponse,
  IdeRunConfiguration,
} from '@deepseek-ai/dsh-client-ui-rainy/ide-execution-protocol'
import { createIdeExecutionService } from '../src/ide-execution.ts'
import { ideExecutionConfigurationSchema, ideExecutionLimitsSchema } from '../src/ide-execution-schema.ts'
import { resolveIdeRun, resolveIdeWorkspacePath, type IdeExecutionFiles } from '../src/ide-execution-resolve.ts'
import { IdeProcessOwner, type IdeSubprocess } from '../src/ide-execution-process.ts'

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (error: Error) => void
  const promise = new Promise<T>((yes, no) => {
    resolve = yes
    reject = no
  })
  return { promise, resolve, reject }
}
const cleanups: (() => Promise<void>)[] = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
  vi.restoreAllMocks()
})
const workspaceId = WorkspaceId('fixture')
const otherWorkspace = WorkspaceId('other')

function filesystem() {
  const kinds = new Map<string, 'file' | 'directory' | 'other'>([
    ['/', 'directory'],
    ['/project', 'directory'],
    ['/other', 'directory'],
    ['/project/main.py', 'file'],
    ['/project/main.c', 'file'],
    ['/project/main.ts', 'file'],
    ['/bundled/tsx.mjs', 'file'],
    ['/project/CMakeLists.txt', 'file'],
  ])
  const canonical = new Map<string, string>()
  const contents = new Map<string, string>()
  const files: IdeExecutionFiles = {
    kind: async path => kinds.get(path),
    realpath: async path => canonical.get(path) ?? path,
    readText: async (path) => {
      const value = contents.get(path)
      if (value === undefined) throw new Error('Missing fixture text.')
      return value
    },
    createBuildDirectory: vi.fn(async (_root: string, path: string) => {
      if (kinds.has(path)) throw new Error('The owned directory already exists.')
      kinds.set(path, 'directory')
    }),
    removeBuildDirectory: vi.fn(async (_root: string, path: string) => {
      kinds.delete(path)
    }),
  }
  return { files, kinds, canonical, contents }
}

function processHandle() {
  const result = deferred<SubprocessOutcome>()
  const stdout = new PassThrough()
  const stderr = new PassThrough()
  const stdin = new PassThrough()
  const quiescence = deferred<boolean>()
  let exited = false
  function finish(exitCode = 0) {
    if (exited) return
    exited = true
    stdout.end()
    stderr.end()
    stdin.end()
    result.resolve({ exitCode, signal: null })
  }
  const handle: SubprocessHandle = {
    stdout,
    stderr,
    stdin,
    control: undefined,
    collected: {},
    done: result.promise,
    terminate: vi.fn(() => {
      finish()
      quiescence.resolve(true)
    }),
    waitForExit: vi.fn(() => quiescence.promise),
  }
  return { handle, finish, stdout, stderr, stdin, quiescence }
}

function terminalHandle() {
  const result = deferred<SubprocessOutcome>()
  const output = new PassThrough()
  const write = vi.fn(async () => {})
  const handle: SubprocessTerminalHandle = {
    pid: 123,
    output,
    done: result.promise,
    write,
    resize: vi.fn(async () => {}),
    inspectForeground: async () => undefined,
    inspectActivity: async () => ({ state: 'unknown', revision: 0 }),
    signalForeground: async () => 123,
    terminate: vi.fn(async () => {
      output.end()
      result.resolve({ exitCode: 0, signal: null })
    }),
  }
  return { handle, result, output, write }
}

function fixture(limits: Record<string, number> = {}) {
  const fs = filesystem()
  const processes: ReturnType<typeof processHandle>[] = []
  const terminals: ReturnType<typeof terminalHandle>[] = []
  const commands: SubprocessSpawnSpec[] = []
  const terminalCommands: SubprocessTerminalSpawnSpec[] = []
  const subprocess: IdeSubprocess = {
    terminalEnvironment: async () => ({ platform: 'posix', defaultShell: '/bin/bash' }),
    resolveExecutable: async name => (name.startsWith('/') ? name : `/usr/bin/${name}`),
    spawn: (spec) => {
      const process = processHandle()
      processes.push(process)
      commands.push(spec)
      return process.handle
    },
    spawnTerminal: async (spec) => {
      const terminal = terminalHandle()
      terminals.push(terminal)
      terminalCommands.push(spec)
      return terminal.handle
    },
  }
  const resources = { resourceRoot: '/bundled', node: '/bundled/node', tsxImport: '/bundled/tsx.mjs' }
  const resolveWorkspace = async (id: typeof workspaceId) => ({ workspaceId: id, root: id === workspaceId ? '/project' : '/other' })
  const options = {
    assertUsable: () => {},
    subprocess,
    resources,
    resolveWorkspace,
    files: fs.files,
    limits: ideExecutionLimitsSchema.parse(limits),
    reportError: vi.fn(),
  }
  const service = createIdeExecutionService(options)
  cleanups.push(() => service.dispose())
  const request = async <T extends IdeExecutionRequest>(input: T): Promise<IdeExecutionResponse<T>> =>
    (await service.handle(input)) as IdeExecutionResponse<T>
  const poll = async (): Promise<IdeExecutionPoll> => request({ op: 'execution.poll', workspaceId, cursor: 0 })
  return {
    ...fs,
    subprocess,
    resources,
    resolveWorkspace,
    options,
    service,
    request,
    poll,
    processes,
    terminals,
    commands,
    terminalCommands,
  }
}

const configuration: IdeRunConfiguration = { name: 'Python', language: 'python', program: 'main.py', terminal: false }

describe('human IDE execution', () => {
  it('rejects new terminal input while the execution target is switching while keeping output and stop available', async () => {
    const f = fixture()
    const terminal = await f.request({ op: 'terminal.start', workspaceId, cols: 80, rows: 24 })
    await vi.waitFor(() => { expect(f.terminals).toHaveLength(1) })
    f.options.assertUsable = () => { throw new Error('Execution target is switching.') }
    await expect(f.request({ op: 'terminal.input', workspaceId, terminalId: terminal.id, data: 'next command\n' }))
      .rejects.toThrow('Execution target is switching.')
    expect(f.terminals[0].write).not.toHaveBeenCalled()
    expect((await f.poll()).status.terminals).toHaveLength(1)
    await expect(f.request({ op: 'terminal.stop', workspaceId, terminalId: terminal.id })).resolves.toEqual({ ok: true })
  })

  it('resolves absolute WSL argv and preserves metacharacters as literal arguments', async () => {
    const f = fixture()
    const resolved = await f.request({
      op: 'run.resolve',
      workspaceId,
      configuration: { ...configuration, arguments: ['a b', '; touch /tmp/never', '$(literal)'], environment: { CUSTOM: 'yes' } },
    })
    expect(resolved.launch).toEqual({
      argv: ['/usr/bin/python3', '-u', '/project/main.py', 'a b', '; touch /tmp/never', '$(literal)'],
      cwd: '/project',
      environment: { CUSTOM: 'yes' },
    })
    expect(f.commands).toHaveLength(0)
    await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, executable: 'python3' } })).rejects.toThrow(
      'absolute native',
    )
    await expect(
      f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, executable: 'C:\\Python\\python.exe' } }),
    ).rejects.toThrow('another execution target')
  })

  it('rejects lexical and symlink escapes, including future output paths', async () => {
    const f = fixture()
    f.kinds.set('/project/link', 'directory')
    f.canonical.set('/project/link', '/outside')
    await expect(resolveIdeWorkspacePath(f.files, '/project', '../outside', 'file')).rejects.toThrow('outside')
    await expect(resolveIdeWorkspacePath(f.files, '/project', 'link/new/program')).rejects.toThrow('outside')
    await expect(
      f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, environment: { DSH_SECRET: 'blocked' } } }),
    ).rejects.toThrow('reserved')
  })

  it('keeps concurrent run identity, output, stop and exit facts independent', async () => {
    const f = fixture()
    const first = await f.request({ op: 'run.start', workspaceId, configuration })
    const second = await f.request({ op: 'run.start', workspaceId, configuration })
    await vi.waitFor(() => {
      expect(f.processes).toHaveLength(2)
    })
    expect(first.id).not.toBe(second.id)
    f.processes[0].stdout.write('first\n')
    f.processes[1].stdout.write('second\n')
    await expect(f.request({ op: 'run.stop', workspaceId: otherWorkspace, runId: first.id })).rejects.toThrow('does not belong')
    await f.request({ op: 'run.stop', workspaceId, runId: first.id })
    const status = (await f.poll()).status
    expect(status.runs.find(run => run.id === first.id)).toMatchObject({ phase: 'exited', exit: { exitCode: 0, stopped: true } })
    expect(status.runs.find(run => run.id === second.id)?.phase).toBe('running')
    f.processes[1].finish(7)
    await vi.waitFor(async () => {
      expect((await f.poll()).status.runs.find(run => run.id === second.id)).toMatchObject({
        phase: 'exited',
        exit: { exitCode: 7, stopped: false },
      })
    })
    const events = (await f.poll()).events.filter(event => event.kind === 'output')
    expect(events).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ operationId: first.id, text: 'first\n' }),
        expect.objectContaining({ operationId: second.id, text: 'second\n' }),
      ]),
    )
  })

  it('never launches a stale CMake executable when the build fails', async () => {
    const f = fixture()
    f.kinds.set('/project/build', 'directory')
    f.kinds.set('/project/build/old', 'file')
    const run = await f.request({
      op: 'run.start',
      workspaceId,
      configuration: {
        name: 'native',
        language: 'c',
        program: 'main.c',
        build: { kind: 'cmake', buildDirectory: 'build', target: 'old', executable: 'old' },
      },
    })
    await vi.waitFor(() => {
      expect(f.processes).toHaveLength(1)
    })
    expect(f.commands[0].argv).toContain('-DCMAKE_BUILD_TYPE=Debug')
    f.processes[0].finish(2)
    await vi.waitFor(async () => {
      expect((await f.poll()).status.runs.find(value => value.id === run.id)).toMatchObject({
        phase: 'failed',
        exit: { exitCode: 2 },
        error: 'The build failed; the program was not launched.',
      })
    })
    expect(f.processes).toHaveLength(1)
    expect(f.terminals).toHaveLength(0)
  })

  it('keeps stop pending until the whole managed range has exited', async () => {
    const f = fixture()
    const run = await f.request({ op: 'run.start', workspaceId, configuration })
    await vi.waitFor(() => {
      expect(f.processes).toHaveLength(1)
    })
    const process = f.processes[0]
    vi.spyOn(process.handle, 'terminate').mockImplementation(() => {
      process.finish()
    })
    const waitForExit = vi.spyOn(process.handle, 'waitForExit')
    let settled = false
    const stop = f.request({ op: 'run.stop', workspaceId, runId: run.id }).then(() => {
      settled = true
    })
    await vi.waitFor(() => {
      expect(waitForExit).toHaveBeenCalled()
    })
    expect(settled).toBe(false)
    expect((await f.poll()).status.runs[0]?.phase).toBe('stopping')
    process.quiescence.resolve(true)
    await stop
    expect((await f.poll()).status.runs[0]).toMatchObject({ phase: 'exited', exit: { exitCode: 0, stopped: true } })
  })

  it('requires a CMake build preset to use the selected contained build directory', async () => {
    const f = fixture()
    f.kinds.set('/project/CMakePresets.json', 'file')
    f.contents.set(
      '/project/CMakePresets.json',
      JSON.stringify({
        version: 6,
        configurePresets: [{ name: 'debug', binaryDir: '${sourceDir}/build' }],
        buildPresets: [{ name: 'build-debug', configurePreset: 'debug' }],
      }),
    )
    const native: IdeRunConfiguration = {
      name: 'CMake',
      language: 'c',
      program: 'main.c',
      build: {
        kind: 'cmake',
        configurePreset: 'debug',
        buildPreset: 'build-debug',
        buildDirectory: 'build',
        executable: 'program',
        target: 'program',
      },
    }
    const valid = await f.request({ op: 'run.resolve', workspaceId, configuration: native })
    expect(valid.build[1]?.argv).toEqual([
      '/usr/bin/cmake',
      '--build',
      '--preset',
      'build-debug',
      '--config',
      'Debug',
      '--target',
      'program',
    ])
    await expect(
      f.request({
        op: 'run.resolve',
        workspaceId,
        configuration: {
          ...native,
          build: {
            kind: 'cmake',
            configurePreset: 'debug',
            buildPreset: 'build-debug',
            buildDirectory: 'elsewhere',
            executable: 'program',
            target: 'program',
          },
        },
      }),
    ).rejects.toThrow('differs')
  })

  it('passes an explicit absolute compiler to CMake and leaves unspecified compilers to the preset', async () => {
    const f = fixture()
    for (const language of ['c', 'cpp'] as const) {
      const native: IdeRunConfiguration = {
        name: 'compiler',
        language,
        program: 'main.c',
        executable: '/project/compiler wrapper',
        build: { kind: 'cmake', buildDirectory: 'build', executable: 'program', target: 'program' },
      }
      const result = await f.request({ op: 'run.resolve', workspaceId, configuration: native })
      expect(result.build[0]?.argv).toContain(`-DCMAKE_${language === 'c' ? 'C' : 'CXX'}_COMPILER=/project/compiler wrapper`)
      const automatic = await f.request({ op: 'run.resolve', workspaceId, configuration: { ...native, executable: undefined } })
      expect(automatic.build[0]?.argv.some(argument => /^-DCMAKE_C(?:XX)?_COMPILER=/.test(argument))).toBe(false)
      await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...native, executable: 'g++' } })).rejects.toThrow(
        'absolute native',
      )
    }
  })

  it('retains UTF-8 output within complete serialized event budgets and reports lost history', async () => {
    const f = fixture({ maxOperations: 1, maxConfigurationBytes: 256, maxEventBytes: 1024, maxOutputBytes: 4096 })
    const run = await f.request({ op: 'run.start', workspaceId, configuration })
    await vi.waitFor(() => {
      expect(f.processes).toHaveLength(1)
    })
    const encoded = Buffer.from('中文🙂'.repeat(1800))
    f.processes[0].stdout.write(encoded.subarray(0, 2))
    f.processes[0].stdout.write(encoded.subarray(2))
    f.processes[0].finish()
    await vi.waitFor(async () => {
      expect((await f.poll()).status.runs[0]?.phase).toBe('exited')
    })
    const result = await f.poll()
    expect(result.truncated).toBe(true)
    expect(result.events.every(event => Buffer.byteLength(JSON.stringify(event)) <= 1024)).toBe(true)
    expect(result.events.reduce((sum, event) => sum + Buffer.byteLength(JSON.stringify(event)), 0)).toBeLessThanOrEqual(4096)
    expect(
      result.events
        .filter(event => event.kind === 'output')
        .map(event => (event.kind === 'output' ? event.text : ''))
        .join(''),
    ).not.toContain('\uFFFD')
    expect(result.status.runs[0]?.id).toBe(run.id)
  })

  it('does not delete a build directory whose exclusive creation failed', async () => {
    const f = fixture()
    vi.spyOn(f.files, 'createBuildDirectory').mockRejectedValue(new Error('already exists'))
    const removeBuildDirectory = vi.spyOn(f.files, 'removeBuildDirectory')
    await f.request({ op: 'run.start', workspaceId, configuration: { name: 'C', language: 'c', program: 'main.c' } })
    await vi.waitFor(async () => {
      expect((await f.poll()).status.runs[0]?.phase).toBe('failed')
    })
    expect(removeBuildDirectory).not.toHaveBeenCalled()
    expect(f.commands).toHaveLength(0)
  })

  it('stops and joins a terminal that finishes allocation after cancellation', async () => {
    const f = fixture()
    const allocation = deferred<SubprocessTerminalHandle>()
    const terminal = terminalHandle()
    const terminate = vi.spyOn(terminal.handle, 'terminate')
    f.subprocess.spawnTerminal = vi.fn(() => allocation.promise)
    const owner = new IdeProcessOwner(f.subprocess, 10, () => {})
    const start = owner.terminal({ argv: ['/bin/bash'], cwd: '/project', environment: {} }, 80, 24)
    const rejected = expect(start).rejects.toThrow('stopped')
    let complete = false
    const close = owner.close().then(() => {
      complete = true
    })
    await Promise.resolve()
    expect(complete).toBe(false)
    allocation.resolve(terminal.handle)
    await rejected
    await close
    expect(terminate).toHaveBeenCalled()
  })

  it('rejects broken profile references and resolves TypeScript through the bundled import', async () => {
    expect(
      ideExecutionConfigurationSchema.safeParse({ profiles: [configuration], activeProfile: 'missing', breakpoints: [], watches: [] })
        .success,
    ).toBe(false)
    const f = fixture()
    const result = await resolveIdeRun({ ...f.options, maxConfigurationBytes: 32768 }, workspaceId, {
      ...configuration,
      language: 'typescript',
      program: 'main.ts',
    })
    expect(result.spec.launch.argv).toEqual(['/bundled/node', '--enable-source-maps', '--import', '/bundled/tsx.mjs', '/project/main.ts'])
  })

  it('uses explicit Python module argv and rejects invalid or cross-language module choices', async () => {
    const f = fixture()
    const module = await f.request({
      op: 'run.resolve',
      workspaceId,
      configuration: { ...configuration, pythonModule: 'package.main', arguments: ['literal value'] },
    })
    expect(module.launch.argv).toEqual(['/usr/bin/python3', '-u', '-m', 'package.main', 'literal value'])
    await expect(
      f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, pythonModule: 'package;bad' } }),
    ).rejects.toThrow('dotted Python')
    await expect(
      f.request({
        op: 'run.resolve',
        workspaceId,
        configuration: { ...configuration, language: 'typescript', program: 'main.ts', pythonModule: 'package.main' },
      }),
    ).rejects.toThrow('require Python')
  })
})
