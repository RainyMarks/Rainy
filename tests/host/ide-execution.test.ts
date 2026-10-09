/** Lifecycle, argv, containment and bounded-history evidence for human runs, terminals and debug launches. */
import { mkdtemp, readdir, readFile, realpath, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { PassThrough } from 'node:stream'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type {
  IdeExecutionPoll, IdeExecutionRequest, IdeExecutionResponse, IdeExecutionStatus, IdeRunConfiguration, IdeRunSnapshot,
} from '../../src/shared/ide-execution-protocol.ts'
import type { WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import type { ProcessHandle, ProcessOutcome, TerminalHandle } from '../../src/host/process.ts'
import { createIdeExecutionService, ideExecutionFailure } from '../../src/host/ide/execution.ts'
import {
  IdeProcessOwner, ideChildEnvironment, localIdeSubprocess, type IdeSpawnSpec, type IdeSubprocess, type IdeTerminalSpawnSpec,
} from '../../src/host/ide/execution-process.ts'
import { resolveIdeRun, resolveIdeWorkspacePath, type IdeExecutionFiles } from '../../src/host/ide/execution-resolve.ts'
import { ideExecutionConfigurationSchema, ideExecutionLimitsSchema } from '../../src/host/ide/execution-schema.ts'
import { getIdeToolPaths } from '../../src/host/ide/tools.ts'
import { ExecutableNotFoundError, resolveExecutable } from '../../src/host/process.ts'

const cleanups: (() => Promise<void>)[] = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
  vi.restoreAllMocks()
})
const workspaceId = brandString<WorkspaceId>('fixture')
const otherWorkspace = brandString<WorkspaceId>('other')

function filesystem() {
  const kinds = new Map<string, 'file' | 'directory' | 'other'>([
    ['/', 'directory'], ['/project', 'directory'], ['/other', 'directory'], ['/project/main.py', 'file'], ['/project/main.c', 'file'],
    ['/project/main.ts', 'file'], ['/bundled/tsx.mjs', 'file'], ['/project/CMakeLists.txt', 'file'],
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
    removeBuildDirectory: vi.fn(async (_root: string, path: string) => { kinds.delete(path) }),
  }
  return { files, kinds, canonical, contents }
}

function processHandle() {
  const result = Promise.withResolvers<ProcessOutcome>()
  const stdout = new PassThrough()
  const stderr = new PassThrough()
  const stdin = new PassThrough()
  const quiescence = Promise.withResolvers<boolean>()
  let exited = false
  function finish(exitCode = 0): void {
    if (exited) return
    exited = true
    stdout.end()
    stderr.end()
    stdin.end()
    result.resolve({ exitCode, signal: null })
  }
  const handle: ProcessHandle = {
    pid: 42, stdout, stderr, stdin, done: result.promise,
    terminate: vi.fn(() => { finish(); quiescence.resolve(true) }),
    waitForExit: vi.fn(() => quiescence.promise),
  }
  return { handle, finish, stdout, stderr, stdin, quiescence }
}

function terminalHandle() {
  const result = Promise.withResolvers<ProcessOutcome>()
  const output = new PassThrough()
  const write = vi.fn(async () => {})
  const handle: TerminalHandle = {
    pid: 123, output, done: result.promise, write, resize: vi.fn(async () => {}),
    terminate: vi.fn(async () => { output.end(); result.resolve({ exitCode: 0, signal: null }) }),
  }
  return { handle, result, output, write }
}

function fixture(limits: Record<string, number> = {}) {
  const fs = filesystem()
  const processes: ReturnType<typeof processHandle>[] = []
  const terminals: ReturnType<typeof terminalHandle>[] = []
  const commands: IdeSpawnSpec[] = []
  const terminalCommands: IdeTerminalSpawnSpec[] = []
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
  const resolveWorkspace = async (id: WorkspaceId) => ({ workspaceId: id, root: id === workspaceId ? '/project' : '/other' })
  const options = {
    assertUsable: () => {}, subprocess, resources, resolveWorkspace, files: fs.files,
    limits: ideExecutionLimitsSchema.parse(limits), reportError: vi.fn(),
  }
  const service = createIdeExecutionService(options)
  cleanups.push(() => service.dispose())
  const request = async <T extends IdeExecutionRequest>(input: T): Promise<IdeExecutionResponse<T>> =>
    (await service.handle(input)) as IdeExecutionResponse<T>
  const poll = async (): Promise<IdeExecutionPoll> => request({ op: 'execution.poll', workspaceId, cursor: 0 })
  return { ...fs, subprocess, resources, options, service, request, poll, processes, terminals, commands, terminalCommands }
}

const configuration: IdeRunConfiguration = { name: 'Python', language: 'python', program: 'main.py', terminal: false }

describe('human IDE execution', () => {
  it('rejects new terminal input while the execution target is switching while keeping output and stop available', async () => {
    const f = fixture()
    const terminal = await f.request({ op: 'terminal.start', workspaceId, cols: 80, rows: 24 })
    await vi.waitFor(() => { expect(f.terminals).toHaveLength(1) })
    expect(f.terminalCommands[0]?.argv).toEqual(['/bin/bash', '--noprofile', '--norc', '-i'])
    f.options.assertUsable = () => { throw new Error('Execution target is switching.') }
    await expect(f.request({ op: 'terminal.input', workspaceId, terminalId: terminal.id, data: 'next command\n' }))
      .rejects.toThrow('Execution target is switching.')
    expect(f.terminals[0]?.write).not.toHaveBeenCalled()
    expect((await f.poll()).status.terminals).toHaveLength(1)
    await expect(f.request({ op: 'terminal.stop', workspaceId, terminalId: terminal.id })).resolves.toEqual({ ok: true })
  })

  it('resolves absolute argv and preserves metacharacters as literal arguments', async () => {
    const f = fixture()
    const resolved = await f.request({
      op: 'run.resolve', workspaceId,
      configuration: { ...configuration, arguments: ['a b', '; touch /tmp/never', '$(literal)'], environment: { CUSTOM: 'yes' } },
    })
    expect(resolved.launch).toEqual({
      argv: ['/usr/bin/python3', '-u', '/project/main.py', 'a b', '; touch /tmp/never', '$(literal)'], cwd: '/project', environment: { CUSTOM: 'yes' },
    })
    expect(f.commands).toHaveLength(0)
    await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, executable: 'python3' } })).rejects.toThrow('absolute native')
    await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, executable: 'C:\\Python\\python.exe' } }))
      .rejects.toThrow('another execution target')
  })

  it('rejects lexical and symlink escapes, including future output paths', async () => {
    const f = fixture()
    f.kinds.set('/project/link', 'directory')
    f.canonical.set('/project/link', '/outside')
    await expect(resolveIdeWorkspacePath(f.files, '/project', '../outside', 'file')).rejects.toThrow('outside')
    await expect(resolveIdeWorkspacePath(f.files, '/project', 'link/new/program')).rejects.toThrow('outside')
    await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, environment: { RAINY_SECRET: 'blocked' } } }))
      .rejects.toThrow('reserved')
  })

  it('keeps concurrent run identity, output, stop and exit facts independent', async () => {
    const f = fixture()
    const first = await f.request({ op: 'run.start', workspaceId, configuration })
    const second = await f.request({ op: 'run.start', workspaceId, configuration })
    await vi.waitFor(() => { expect(f.processes).toHaveLength(2) })
    expect(first.id).not.toBe(second.id)
    f.processes[0]!.stdout.write('first\n')
    f.processes[1]!.stdout.write('second\n')
    await expect(f.request({ op: 'run.stop', workspaceId: otherWorkspace, runId: first.id })).rejects.toThrow('does not belong')
    await f.request({ op: 'run.stop', workspaceId, runId: first.id })
    const status = (await f.poll()).status
    expect(status.runs.find(run => run.id === first.id)).toMatchObject({ phase: 'exited', exit: { exitCode: 0, stopped: true } })
    expect(status.runs.find(run => run.id === second.id)?.phase).toBe('running')
    f.processes[1]!.finish(7)
    await vi.waitFor(async () => {
      expect((await f.poll()).status.runs.find(run => run.id === second.id)).toMatchObject({ phase: 'exited', exit: { exitCode: 7, stopped: false } })
    })
    const events = (await f.poll()).events.filter(event => event.kind === 'output')
    expect(events).toEqual(expect.arrayContaining([
      expect.objectContaining({ operationId: first.id, text: 'first\n' }),
      expect.objectContaining({ operationId: second.id, text: 'second\n' }),
    ]))
  })

  it('stops every operation of one project for project removal', async () => {
    const f = fixture()
    const run = await f.request({ op: 'run.start', workspaceId, configuration })
    const terminal = await f.request({ op: 'terminal.start', workspaceId, cols: 80, rows: 24 })
    await vi.waitFor(() => { expect(f.processes).toHaveLength(1); expect(f.terminals).toHaveLength(1) })
    expect(f.service.hasActivity()).toBe(true)
    await f.service.stopWorkspace(workspaceId)
    const status = (await f.poll()).status
    expect(status.runs.find(value => value.id === run.id)).toBeUndefined()
    expect(status.terminals.find(value => value.id === terminal.id)).toBeUndefined()
    expect(f.processes[0]?.handle.terminate).toHaveBeenCalled()
    expect(f.service.hasActivity()).toBe(false)
  })

  it('never launches a stale CMake executable when the build fails', async () => {
    const f = fixture()
    f.kinds.set('/project/build', 'directory')
    f.kinds.set('/project/build/old', 'file')
    const run = await f.request({ op: 'run.start', workspaceId, configuration: {
      name: 'native', language: 'c', program: 'main.c', build: { kind: 'cmake', buildDirectory: 'build', target: 'old', executable: 'old' },
    } })
    await vi.waitFor(() => { expect(f.processes).toHaveLength(1) })
    expect(f.commands[0]?.argv).toContain('-DCMAKE_BUILD_TYPE=Debug')
    f.processes[0]!.finish(2)
    await vi.waitFor(async () => {
      expect((await f.poll()).status.runs.find(value => value.id === run.id)).toMatchObject({
        phase: 'failed', exit: { exitCode: 2 }, error: 'The build failed; the program was not launched.',
      })
    })
    expect(f.processes).toHaveLength(1)
    expect(f.terminals).toHaveLength(0)
  })

  it('keeps stop pending until the whole process tree has exited', async () => {
    const f = fixture()
    const run = await f.request({ op: 'run.start', workspaceId, configuration })
    await vi.waitFor(() => { expect(f.processes).toHaveLength(1) })
    const process = f.processes[0]!
    vi.spyOn(process.handle, 'terminate').mockImplementation(() => { process.finish() })
    const waitForExit = vi.spyOn(process.handle, 'waitForExit')
    let settled = false
    const stop = f.request({ op: 'run.stop', workspaceId, runId: run.id }).then(() => { settled = true })
    await vi.waitFor(() => { expect(waitForExit).toHaveBeenCalled() })
    expect(settled).toBe(false)
    expect((await f.poll()).status.runs[0]?.phase).toBe('stopping')
    process.quiescence.resolve(true)
    await stop
    expect((await f.poll()).status.runs[0]).toMatchObject({ phase: 'exited', exit: { exitCode: 0, stopped: true } })
  })

  it('requires a CMake build preset to use the selected contained build directory', async () => {
    const f = fixture()
    f.kinds.set('/project/CMakePresets.json', 'file')
    f.contents.set('/project/CMakePresets.json', JSON.stringify({
      version: 6, configurePresets: [{ name: 'debug', binaryDir: '${sourceDir}/build' }], buildPresets: [{ name: 'build-debug', configurePreset: 'debug' }],
    }))
    const native: IdeRunConfiguration = { name: 'CMake', language: 'c', program: 'main.c',
      build: { kind: 'cmake', configurePreset: 'debug', buildPreset: 'build-debug', buildDirectory: 'build', executable: 'program', target: 'program' } }
    const valid = await f.request({ op: 'run.resolve', workspaceId, configuration: native })
    expect(valid.build[1]?.argv).toEqual(['/usr/bin/cmake', '--build', '--preset', 'build-debug', '--config', 'Debug', '--target', 'program'])
    await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...native,
      build: { kind: 'cmake', configurePreset: 'debug', buildPreset: 'build-debug', buildDirectory: 'elsewhere', executable: 'program', target: 'program' } } }))
      .rejects.toThrow('differs')
  })

  it('passes an explicit absolute compiler to CMake and leaves unspecified compilers to the preset', async () => {
    const f = fixture()
    for (const language of ['c', 'cpp'] as const) {
      const native: IdeRunConfiguration = { name: 'compiler', language, program: 'main.c', executable: '/project/compiler wrapper',
        build: { kind: 'cmake', buildDirectory: 'build', executable: 'program', target: 'program' } }
      const result = await f.request({ op: 'run.resolve', workspaceId, configuration: native })
      expect(result.build[0]?.argv).toContain(`-DCMAKE_${language === 'c' ? 'C' : 'CXX'}_COMPILER=/project/compiler wrapper`)
      const automatic = await f.request({ op: 'run.resolve', workspaceId, configuration: { ...native, executable: undefined } })
      expect(automatic.build[0]?.argv.some(argument => /^-DCMAKE_C(?:XX)?_COMPILER=/.test(argument))).toBe(false)
      await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...native, executable: 'g++' } })).rejects.toThrow('absolute native')
    }
  })

  it('retains UTF-8 output within complete serialized event budgets and reports lost history', async () => {
    const f = fixture({ maxOperations: 1, maxConfigurationBytes: 256, maxEventBytes: 1024, maxOutputBytes: 4096 })
    const run = await f.request({ op: 'run.start', workspaceId, configuration })
    await vi.waitFor(() => { expect(f.processes).toHaveLength(1) })
    const encoded = Buffer.from('中文🙂'.repeat(1800))
    f.processes[0]!.stdout.write(encoded.subarray(0, 2))
    f.processes[0]!.stdout.write(encoded.subarray(2))
    f.processes[0]!.finish()
    await vi.waitFor(async () => { expect((await f.poll()).status.runs[0]?.phase).toBe('exited') })
    const result = await f.poll()
    expect(result.truncated).toBe(true)
    expect(result.events.every(event => Buffer.byteLength(JSON.stringify(event)) <= 1024)).toBe(true)
    expect(result.events.reduce((sum, event) => sum + Buffer.byteLength(JSON.stringify(event)), 0)).toBeLessThanOrEqual(4096)
    expect(result.events.filter(event => event.kind === 'output').map(event => (event.kind === 'output' ? event.text : '')).join('')).not.toContain('\uFFFD')
    expect(result.status.runs[0]?.id).toBe(run.id)
  })

  it('does not delete a build directory whose exclusive creation failed', async () => {
    const f = fixture()
    vi.spyOn(f.files, 'createBuildDirectory').mockRejectedValue(new Error('already exists'))
    const removeBuildDirectory = vi.spyOn(f.files, 'removeBuildDirectory')
    await f.request({ op: 'run.start', workspaceId, configuration: { name: 'C', language: 'c', program: 'main.c' } })
    await vi.waitFor(async () => { expect((await f.poll()).status.runs[0]?.phase).toBe('failed') })
    expect(removeBuildDirectory).not.toHaveBeenCalled()
    expect(f.commands).toHaveLength(0)
  })

  it('stops and joins a terminal that finishes allocation after cancellation', async () => {
    const f = fixture()
    const allocation = Promise.withResolvers<TerminalHandle>()
    const terminal = terminalHandle()
    const terminate = vi.spyOn(terminal.handle, 'terminate')
    f.subprocess.spawnTerminal = vi.fn(() => allocation.promise)
    const owner = new IdeProcessOwner(f.subprocess, 10, () => {})
    const start = owner.terminal({ argv: ['/bin/bash'], cwd: '/project', environment: {} }, 80, 24)
    const rejected = expect(start).rejects.toThrow('stopped')
    let complete = false
    const close = owner.close().then(() => { complete = true })
    await Promise.resolve()
    expect(complete).toBe(false)
    allocation.resolve(terminal.handle)
    await rejected
    await close
    expect(terminate).toHaveBeenCalled()
  })

  it('rejects broken profile references and resolves TypeScript through the bundled import', async () => {
    expect(ideExecutionConfigurationSchema.safeParse({ profiles: [configuration], activeProfile: 'missing', breakpoints: [], watches: [] }).success).toBe(false)
    const f = fixture()
    const result = await resolveIdeRun({ ...f.options, maxConfigurationBytes: 32768 }, workspaceId, { ...configuration, language: 'typescript', program: 'main.ts' })
    expect(result.spec.launch.argv).toEqual(['/bundled/node', '--enable-source-maps', '--import', '/bundled/tsx.mjs', '/project/main.ts'])
  })

  it('uses explicit Python module argv and rejects invalid or cross-language module choices', async () => {
    const f = fixture()
    const module = await f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, pythonModule: 'package.main', arguments: ['literal value'] } })
    expect(module.launch.argv).toEqual(['/usr/bin/python3', '-u', '-m', 'package.main', 'literal value'])
    await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, pythonModule: 'package;bad' } })).rejects.toThrow('dotted Python')
    await expect(f.request({ op: 'run.resolve', workspaceId, configuration: { ...configuration, language: 'typescript', program: 'main.ts', pythonModule: 'package.main' } }))
      .rejects.toThrow('require Python')
  })

  it('refuses PHP debugging and classifies dependency failures', async () => {
    const f = fixture()
    await expect(f.request({ op: 'debug.start', workspaceId, configuration: { ...configuration, language: 'php', program: 'main.py' }, breakpoints: [] }))
      .rejects.toThrow('no PHP debug adapter')
    expect(ideExecutionFailure(new ExecutableNotFoundError('gdb'))).toEqual({ code: 'dependency-unavailable', message: 'Executable not found: gdb' })
    expect(ideExecutionFailure(new Error('other'))).toEqual({ code: 'execution-error', message: 'other' })
  })
})

describe('local process provider', () => {
  it('layers an overlay over the scrubbed Host environment', () => {
    process.env.RAINY_TEST_API_KEY = 'secret'
    try {
      const environment = ideChildEnvironment({ CUSTOM: 'yes' })
      expect(environment.CUSTOM).toBe('yes')
      expect(environment.RAINY_TEST_API_KEY).toBeUndefined()
      expect(environment.PATH).toBe(process.env.PATH)
    } finally { delete process.env.RAINY_TEST_API_KEY }
  })

  it('resolves PATH programs and rejects relative paths with separators', async () => {
    await expect(localIdeSubprocess.resolveExecutable(process.platform === 'win32' ? 'cmd' : 'sh')).resolves.toMatch(/sh|cmd/i)
    await expect(localIdeSubprocess.resolveExecutable('definitely-not-a-rainy-program')).rejects.toBeInstanceOf(ExecutableNotFoundError)
    await expect(localIdeSubprocess.resolveExecutable('bin/tool')).rejects.toThrow('relative path')
  })
})

describe.skipIf(process.platform === 'win32')('real processes', () => {
  async function real() {
    const root = await realpath(await mkdtemp(join(tmpdir(), 'rainy-ide-run-')))
    cleanups.push(() => rm(root, { recursive: true, force: true }))
    const tools = await getIdeToolPaths(join(process.cwd(), 'resources'))
    const service = createIdeExecutionService({
      subprocess: localIdeSubprocess, assertUsable: () => {}, reportError: () => {},
      resources: { resourceRoot: tools.resourceRoot, node: tools.node, tsxImport: tools.tsxImport },
      resolveWorkspace: async id => ({ workspaceId: id, root }),
      resolveEnvironment: () => ({ environment: { RAINY_TEST_MARKER: 'from-runtime' }, executables: {} }),
      limits: ideExecutionLimitsSchema.parse({}),
    })
    cleanups.push(() => service.dispose())
    const output = async (): Promise<string> => {
      const poll = await service.handle({ op: 'execution.poll', workspaceId, cursor: 0 }) as IdeExecutionPoll
      return poll.events.map(event => event.kind === 'output' ? event.text : '').join('')
    }
    const status = async () => (await service.handle({ op: 'execution.status', workspaceId })) as IdeExecutionStatus
    return { root, service, output, status }
  }

  it('runs a Node program with piped output and the runtime environment', async () => {
    const { root, service, output, status } = await real()
    await writeFile(join(root, 'main.js'), 'console.log("hello", process.env.RAINY_TEST_MARKER, process.argv[2]); console.error("warn")\n')
    await service.handle({ op: 'run.start', workspaceId, configuration: { name: 'node', language: 'javascript', program: 'main.js', arguments: ['a b'], terminal: false } })
    await vi.waitFor(async () => { expect((await status()).runs[0]?.phase).toBe('exited') }, { timeout: 15000 })
    expect((await status()).runs[0]?.exit).toEqual({ exitCode: 0, signal: null, stopped: false })
    expect(await output()).toContain('hello from-runtime a b')
    expect(await output()).toContain('warn')
  })

  it('runs a TypeScript program in a terminal through the bundled tsx loader and forwards input', async () => {
    const { root, service, output, status } = await real()
    await writeFile(join(root, 'main.ts'), [
      'const name: string = "typed"',
      'process.stdout.write(`ready ${name}\\n`)',
      'process.stdin.once("data", (data) => { process.stdout.write(`got ${String(data).trim()}\\n`); process.exit(3) })',
    ].join('\n'))
    const run = await service.handle({ op: 'run.start', workspaceId, configuration: { name: 'ts', language: 'typescript', program: 'main.ts' } }) as { id: string }
    await vi.waitFor(async () => { expect(await output()).toContain('ready typed') }, { timeout: 15000 })
    await service.handle({ op: 'run.input', workspaceId, runId: run.id, data: 'payload\r' })
    await vi.waitFor(async () => { expect((await status()).runs[0]?.phase).toBe('exited') }, { timeout: 15000 })
    expect(await output()).toContain('got payload')
    expect((await status()).runs[0]?.exit?.exitCode).toBe(3)
  })

  it('compiles a single C file into an owned build directory, runs it and removes the build directory', async () => {
    const { root, service, output, status } = await real()
    await writeFile(join(root, 'main.c'), '#include <stdio.h>\nint main(void) { printf("%d\\n", 40 + 2); return 7; }\n')
    const run = await service.handle({ op: 'run.start', workspaceId, configuration: { name: 'c', language: 'c', program: 'main.c', terminal: false } }) as IdeRunSnapshot
    expect(run.spec.build[0]?.argv.slice(1, 4)).toEqual(['-g', '-O0', '-fno-omit-frame-pointer'])
    await vi.waitFor(async () => { expect((await status()).runs[0]?.phase).toBe('exited') }, { timeout: 30000 })
    expect((await status()).runs[0]?.exit?.exitCode).toBe(7)
    expect(await output()).toContain('42')
    expect(await readdir(join(root, '.rainy-ide', 'build'))).toEqual([])
  }, 60000)

  it.skipIf(resolveExecutable('cmake') === undefined || resolveExecutable('g++') === undefined)('builds a CMake target with an explicitly chosen compiler', async () => {
    const { root, service, output, status } = await real()
    const wrapper = join(root, 'chosen compiler')
    await writeFile(wrapper, `#!/bin/sh\nexec '${resolveExecutable('g++')!.replaceAll("'", "'\\''")}' "$@"\n`, { mode: 0o700 })
    await writeFile(join(root, 'main.cpp'), '#include <iostream>\nint main() { std::cout << 42 << std::endl; }\n')
    await writeFile(join(root, 'CMakeLists.txt'), 'cmake_minimum_required(VERSION 3.20)\nproject(CompilerSelection CXX)\nadd_executable(main main.cpp)\n')
    const run = await service.handle({ op: 'run.start', workspaceId, configuration: { name: 'cmake', language: 'cpp', program: 'main.cpp',
      executable: wrapper, terminal: false, build: { kind: 'cmake', buildDirectory: 'build', executable: 'main', target: 'main' } } }) as IdeRunSnapshot
    expect(run.spec.build[0]?.argv).toContain(`-DCMAKE_CXX_COMPILER=${wrapper}`)
    await vi.waitFor(async () => {
      const current = (await status()).runs[0]
      if (current?.phase === 'failed') throw new Error(current.error)
      expect(current?.phase).toBe('exited')
    }, { timeout: 120000, interval: 100 })
    expect((await status()).runs[0]?.exit?.exitCode).toBe(0)
    expect(await output()).toMatch(/(?:^|\n)42\r?\n/u)
    expect((await readFile(join(root, 'build', 'CMakeCache.txt'), 'utf8')).split('\n').find(line => line.startsWith('CMAKE_CXX_COMPILER:'))).toMatch(new RegExp(`=${wrapper}$`, 'u'))
  }, 180000)

  it('starts an interactive Bash terminal, runs a command and stops it', async () => {
    const { root, service, output, status } = await real()
    const terminal = await service.handle({ op: 'terminal.start', workspaceId, cols: 80, rows: 24 }) as { id: string; cwd: string }
    expect(terminal.cwd).toBe(root)
    await vi.waitFor(async () => { expect((await status()).terminals[0]?.phase).toBe('running') }, { timeout: 15000 })
    await service.handle({ op: 'terminal.input', workspaceId, terminalId: terminal.id, data: 'echo "$RAINY_TEST_MARKER-$((40+2))"\n' })
    await vi.waitFor(async () => { expect(await output()).toContain('from-runtime-42') }, { timeout: 15000 })
    await service.handle({ op: 'terminal.resize', workspaceId, terminalId: terminal.id, cols: 100, rows: 30 })
    await service.handle({ op: 'terminal.stop', workspaceId, terminalId: terminal.id })
    expect((await status()).terminals[0]).toMatchObject({ phase: 'exited', exit: { stopped: true } })
    expect(service.hasActivity()).toBe(false)
  })
})
