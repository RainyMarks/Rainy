/** Real WSL acceptance fixture, bundled by the desktop build and run with its private Node and dependencies. */
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import assert from 'node:assert/strict'
import { Context } from '@deepseek-ai/cordis'
import LocalSubprocessRuntime from '@deepseek-ai/dsh-subprocess-local'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type { SubprocessHandle, SubprocessTerminalHandle } from '@deepseek-ai/dsh-subprocess'
import type {
  IdeDebugId,
  IdeDebugSnapshot,
  IdeExecutionLanguage,
  IdeExecutionPoll,
  IdeExecutionRequest,
  IdeExecutionResponse,
  IdeRunConfiguration,
  IdeRunSnapshot,
} from '@deepseek-ai/dsh-client-ui-rainy/ide-execution-protocol'
import { createIdeExecutionService } from '../src/ide-execution.ts'
import { ideExecutionLimitsSchema } from '../src/ide-execution-schema.ts'
import { getIdeToolPaths } from '../src/ide-tools.ts'
import type { IdeSubprocess } from '../src/ide-execution-process.ts'

const root = await mkdtemp(join(tmpdir(), 'rainy-ide-execution-'))
const workspaceId = WorkspaceId('ide-execution-smoke')
const context = new Context()
await context.plugin(LocalSubprocessRuntime)
const failures: string[] = []
const handles: SubprocessHandle[] = []
const terminals: SubprocessTerminalHandle[] = []
const subprocess: IdeSubprocess = {
  resolveExecutable: (name, environment) => context.subprocess.resolveExecutable(name, environment),
  terminalEnvironment: signal => context.subprocess.terminalEnvironment(signal),
  spawn: (spec) => {
    const handle = context.subprocess.spawn(spec)
    handles.push(handle)
    return handle
  },
  spawnTerminal: async (spec) => {
    const terminal = await context.subprocess.spawnTerminal(spec)
    terminals.push(terminal)
    return terminal
  },
}
const service = createIdeExecutionService({
  assertUsable: () => {},
  subprocess,
  resources: getIdeToolPaths(),
  resolveWorkspace: async (id) => {
    assert.equal(id, workspaceId)
    return { workspaceId, root }
  },
  limits: ideExecutionLimitsSchema.parse({}),
  reportError: (error) => {
    failures.push(String(error))
  },
})

async function request<T extends IdeExecutionRequest>(value: T): Promise<IdeExecutionResponse<T>> {
  return (await service.handle(value)) as IdeExecutionResponse<T>
}
async function poll(): Promise<IdeExecutionPoll> {
  return request({ op: 'execution.poll', workspaceId, cursor: 0 })
}
async function until<T>(read: () => Promise<T | undefined>, message: string): Promise<T> {
  const deadline = Date.now() + 65000
  while (Date.now() < deadline) {
    const value = await read()
    if (value !== undefined) return value
    await delay(25)
  }
  throw new Error(`${message}\n${JSON.stringify(await poll())}`)
}
async function paused(id: IdeDebugId): Promise<IdeDebugSnapshot> {
  return until(async () => {
    const state = (await poll()).status.debugSessions.find(value => value.id === id)
    if (state?.phase === 'failed' || state?.phase === 'terminated')
      throw new Error(`Debug ended before breakpoint: ${JSON.stringify(await poll())}`)
    return state?.phase === 'paused' ? state : undefined
  }, 'Waiting for a real debugger breakpoint timed out.')
}
async function pauseAfter(id: IdeDebugId, cursor: number): Promise<IdeDebugSnapshot> {
  return until(async () => {
    const result = await poll()
    const state = result.status.debugSessions.find(value => value.id === id)
    if (state?.phase === 'failed' || state?.phase === 'terminated')
      throw new Error(`Debug ended during a control: ${JSON.stringify(result)}`)
    return state?.phase === 'paused' &&
      result.events.some(
        event => event.sequence > cursor && event.kind === 'debug' && event.debug.id === id && event.debug.phase === 'paused',
      )
      ? state
      : undefined
  }, 'The debugger did not acknowledge its next stop.')
}
async function frame(id: IdeDebugId) {
  const stopped = await paused(id)
  const threads = await request({ op: 'debug.threads', workspaceId, debugId: id })
  const threadId = stopped.threadId ?? threads[0]?.id
  assert.notEqual(threadId, undefined)
  const frames = await request({ op: 'debug.stack', workspaceId, debugId: id, threadId: threadId })
  assert(frames[0])
  return { stopped, threadId: threadId, frames, top: frames[0] }
}

const cases: { language: IdeExecutionLanguage; file: string; source: string; line: number }[] = [
  { language: 'python', file: 'hello.py', source: 'x = 41\nx += 1\nprint(x)\n', line: 2 },
  { language: 'javascript', file: 'hello.js', source: 'const x = 41;\nconst y = x + 1;\nconsole.log(y);\n', line: 2 },
  { language: 'typescript', file: 'hello.ts', source: 'const x: number = 41;\nconst y: number = x + 1;\nconsole.log(y);\n', line: 2 },
  {
    language: 'c',
    file: 'hello.c',
    source: '#include <stdio.h>\nint main(void) {\n  int x = 41;\n  x += 1;\n  printf("%d\\n", x);\n  return 0;\n}\n',
    line: 4,
  },
  {
    language: 'cpp',
    file: 'hello.cpp',
    source: '#include <iostream>\nint main() {\n  int x = 41;\n  x += 1;\n  std::cout << x << std::endl;\n  return 0;\n}\n',
    line: 4,
  },
]
const results: Record<string, unknown>[] = []
try {
  for (const test of cases) await writeFile(join(root, test.file), test.source)
  for (const test of cases) {
    const configuration: IdeRunConfiguration = { name: test.language, language: test.language, program: test.file, terminal: false }
    const run = await request({ op: 'run.start', workspaceId, configuration })
    const exited: IdeRunSnapshot = await until(async () => {
      const state = (await poll()).status.runs.find(value => value.id === run.id)
      if (state?.phase === 'failed') throw new Error(`Run failed: ${JSON.stringify(await poll())}`)
      return state?.phase === 'exited' ? state : undefined
    }, 'Waiting for a real program exit timed out.')
    assert.equal(exited.exit?.exitCode, 0)
    assert.match(
      (await poll()).events
        .filter(event => event.kind === 'output' && event.operationId === run.id)
        .map(event => (event.kind === 'output' ? event.text : ''))
        .join(''),
      /42/,
    )
    const debug = await request({
      op: 'debug.start',
      workspaceId,
      configuration: { ...configuration, terminal: true },
      breakpoints: [{ path: test.file, lines: [test.line] }],
    })
    await paused(debug.id)
    const stopped = await until(
      async () =>
        (await poll()).status.debugSessions.find(
          value =>
            value.id === debug.id &&
            value.phase === 'paused' &&
            value.breakpoints.some(breakpoint => breakpoint.verified && breakpoint.line === test.line),
        ),
      'The adapter did not verify its breakpoint.',
    )
    assert(
      stopped.breakpoints.some(breakpoint => breakpoint.verified && breakpoint.line === test.line),
      JSON.stringify(stopped.breakpoints),
    )
    const threads = await request({ op: 'debug.threads', workspaceId, debugId: debug.id })
    const threadId = stopped.threadId ?? threads[0]?.id
    assert.notEqual(threadId, undefined)
    const frames = await request({ op: 'debug.stack', workspaceId, debugId: debug.id, threadId: threadId })
    assert(frames.length > 0)
    assert.equal(frames[0]?.path, join(root, test.file))
    assert.equal(frames[0]?.line, test.line)
    const scopes = await request({ op: 'debug.scopes', workspaceId, debugId: debug.id, frameId: frames[0].id })
    assert(scopes.length > 0)
    const variables = await request({
      op: 'debug.variables',
      workspaceId,
      debugId: debug.id,
      variablesReference: scopes[0].variablesReference,
    })
    assert(Array.isArray(variables))
    const watch = await request({
      op: 'debug.evaluate',
      workspaceId,
      debugId: debug.id,
      expression: 'x',
      context: 'watch',
      frameId: frames[0].id,
    })
    assert.equal(watch.result, '41')
    const beforeStep = (await poll()).cursor
    await request({ op: 'debug.control', workspaceId, debugId: debug.id, action: 'next', threadId: threadId })
    await pauseAfter(debug.id, beforeStep)
    await until(async () => {
      const current = await paused(debug.id)
      const stack = await request({ op: 'debug.stack', workspaceId, debugId: debug.id, threadId: current.threadId ?? threadId })
      return stack[0]?.line !== test.line ? stack : undefined
    }, 'The debugger did not step to the next line.')
    await request({ op: 'debug.control', workspaceId, debugId: debug.id, action: 'continue', threadId: threadId })
    const completed = await until(async () => {
      const state = (await poll()).status.debugSessions.find(value => value.id === debug.id)
      if (state?.phase === 'failed') throw new Error(`Debug completion failed: ${JSON.stringify(await poll())}`)
      return state?.phase === 'terminated' ? state : undefined
    }, 'Waiting for debug cleanup timed out.')
    assert.equal(completed.exit?.exitCode, 0)
    results.push({
      language: test.language,
      runExit: exited.exit,
      breakpoint: test.line,
      watch: watch.result,
      scopes: scopes.length,
      step: true,
      debugExit: completed.exit,
    })
    console.log(JSON.stringify(results.at(-1)))
  }
  await writeFile(
    join(root, 'CMakeLists.txt'),
    'cmake_minimum_required(VERSION 3.20)\nproject(RainySmoke C)\nadd_executable(hello hello.c)\n',
  )
  await mkdir(join(root, 'build'))
  const cmake = await request({
    op: 'run.start',
    workspaceId,
    configuration: {
      name: 'cmake',
      language: 'c',
      program: 'hello.c',
      terminal: false,
      build: { kind: 'cmake', buildDirectory: 'build', target: 'hello', executable: 'hello' },
    },
  })
  const compiled = await until(async () => {
    const state = (await poll()).status.runs.find(value => value.id === cmake.id)
    if (state?.phase === 'failed') throw new Error(`CMake failed: ${JSON.stringify(await poll())}`)
    return state?.phase === 'exited' ? state : undefined
  }, 'CMake run did not finish.')
  assert.equal(compiled.exit?.exitCode, 0)
  assert.match(await readFile(join(root, 'build/compile_commands.json'), 'utf8'), /hello\.c/)
  const cmakeDebug = await request({
    op: 'debug.start',
    workspaceId,
    configuration: {
      name: 'cmake-debug',
      language: 'c',
      program: 'hello.c',
      terminal: true,
      build: { kind: 'cmake', buildDirectory: 'build', target: 'hello', executable: 'hello' },
    },
    breakpoints: [{ path: 'hello.c', lines: [4] }],
  })
  const cmakeFrame = await frame(cmakeDebug.id)
  assert(cmakeFrame.stopped.breakpoints.some(breakpoint => breakpoint.verified && breakpoint.line === 4))
  assert.equal(
    (
      await request({
        op: 'debug.evaluate',
        workspaceId,
        debugId: cmakeDebug.id,
        expression: 'x',
        context: 'watch',
        frameId: cmakeFrame.top.id,
      })
    ).result,
    '41',
  )
  await request({ op: 'debug.control', workspaceId, debugId: cmakeDebug.id, action: 'continue', threadId: cmakeFrame.threadId })
  await until(
    async () => (await poll()).status.debugSessions.find(value => value.id === cmakeDebug.id && value.phase === 'terminated'),
    'The CMake debuggee did not exit.',
  )
  results.push({ cmakeDebugBuild: true, compileCommands: true, breakpoint: 4, watch: '41', exit: compiled.exit })
  console.log(JSON.stringify(results.at(-1)))
  await writeFile(join(root, 'module_entry.py'), 'x = 41\nx += 1\nprint(x)\n')
  const moduleConfiguration: IdeRunConfiguration = {
    name: 'module',
    language: 'python',
    program: 'module_entry.py',
    pythonModule: 'module_entry',
    terminal: false,
  }
  const moduleRun = await request({ op: 'run.start', workspaceId, configuration: moduleConfiguration })
  const moduleExit = await until(
    async () =>
      (await poll()).status.runs.find(value => value.id === moduleRun.id && (value.phase === 'exited' || value.phase === 'failed')),
    'The Python module did not exit.',
  )
  assert.equal(moduleExit.exit?.exitCode, 0, JSON.stringify(moduleExit))
  const moduleDebug = await request({
    op: 'debug.start',
    workspaceId,
    configuration: { ...moduleConfiguration, terminal: true },
    breakpoints: [{ path: 'module_entry.py', lines: [2] }],
  })
  const moduleFrame = await frame(moduleDebug.id)
  assert(moduleFrame.stopped.breakpoints.some(breakpoint => breakpoint.verified && breakpoint.line === 2))
  assert.equal(
    (
      await request({
        op: 'debug.evaluate',
        workspaceId,
        debugId: moduleDebug.id,
        expression: 'x',
        context: 'watch',
        frameId: moduleFrame.top.id,
      })
    ).result,
    '41',
  )
  await request({ op: 'debug.control', workspaceId, debugId: moduleDebug.id, action: 'continue', threadId: moduleFrame.threadId })
  await until(
    async () => (await poll()).status.debugSessions.find(value => value.id === moduleDebug.id && value.phase === 'terminated'),
    'The Python module debugger did not exit.',
  )
  results.push({ pythonModuleRun: true, pythonModuleDebug: true, watch: '41' })
  console.log(JSON.stringify(results.at(-1)))
  const controls: { language: IdeExecutionLanguage; file: string; source: string; line: number }[] = [
    {
      language: 'python',
      file: 'controls.py',
      source:
        'import time\ndef increment(value):\n    result = value + 1\n    return result\nx = 41\nx = increment(x)\nprint(x, flush=True)\nwhile True:\n    time.sleep(0.05)\n',
      line: 6,
    },
    {
      language: 'javascript',
      file: 'controls.js',
      source:
        'function increment(value) {\n  const result = value + 1;\n  return result;\n}\nconst x = 41;\nconst y = increment(x);\nconsole.log(y);\nsetInterval(() => {}, 50);\n',
      line: 6,
    },
    {
      language: 'c',
      file: 'controls.c',
      source:
        '#include <stdio.h>\n#include <unistd.h>\nint increment(int value) {\n  int result = value + 1;\n  return result;\n}\nint main(void) {\n  int x = 41;\n  int y = increment(x);\n  printf("%d\\n", y);\n  while (1) { usleep(50000); }\n}\n',
      line: 9,
    },
  ]
  for (const test of controls) {
    await writeFile(join(root, test.file), test.source)
    const debug = await request({
      op: 'debug.start',
      workspaceId,
      configuration: { name: `controls-${test.language}`, language: test.language, program: test.file, terminal: true },
      breakpoints: [{ path: test.file, lines: [test.line] }],
    })
    const before = await frame(debug.id)
    let cursor = (await poll()).cursor
    await request({ op: 'debug.control', workspaceId, debugId: debug.id, action: 'stepIn', threadId: before.threadId })
    await pauseAfter(debug.id, cursor)
    const entered = await frame(debug.id)
    assert.match(entered.top.name, /increment/)
    assert.equal(
      (
        await request({
          op: 'debug.evaluate',
          workspaceId,
          debugId: debug.id,
          expression: 'value',
          context: 'watch',
          frameId: entered.top.id,
        })
      ).result,
      '41',
    )
    cursor = (await poll()).cursor
    await request({ op: 'debug.control', workspaceId, debugId: debug.id, action: 'stepOut', threadId: entered.threadId })
    await pauseAfter(debug.id, cursor)
    const returned = await frame(debug.id)
    assert.doesNotMatch(returned.top.name, /increment/)
    await request({ op: 'debug.control', workspaceId, debugId: debug.id, action: 'continue', threadId: returned.threadId })
    await until(
      async () => (await poll()).status.debugSessions.find(value => value.id === debug.id && value.phase === 'running'),
      'The loop did not resume.',
    )
    cursor = (await poll()).cursor
    await request({ op: 'debug.control', workspaceId, debugId: debug.id, action: 'pause', threadId: returned.threadId })
    await pauseAfter(debug.id, cursor)
    await request({ op: 'debug.stop', workspaceId, debugId: debug.id })
    assert.equal((await poll()).status.debugSessions.find(value => value.id === debug.id)?.exit?.stopped, true)
    results.push({ language: test.language, stepIn: true, stepOut: true, pause: true, stop: true })
    console.log(JSON.stringify(results.at(-1)))
  }
  const terminal = await request({ op: 'terminal.start', workspaceId, cols: 80, rows: 24 })
  await until(
    async () => (await poll()).status.terminals.find(value => value.id === terminal.id && value.phase === 'running'),
    'The workspace terminal did not start.',
  )
  await request({ op: 'terminal.resize', workspaceId, terminalId: terminal.id, cols: 132, rows: 40 })
  await request({ op: 'terminal.input', workspaceId, terminalId: terminal.id, data: "stty size; printf '%s\\n' RAINY_TERMINAL_OK\n" })
  await until(async () => {
    const text = (await poll()).events
      .flatMap(event => (event.kind === 'output' && event.operationId === terminal.id ? [event.text] : []))
      .join('')
    return /\r?\nRAINY_TERMINAL_OK\r?\n/.test(text) && text.includes('40 132\r\n') ? true : undefined
  }, 'The workspace terminal did not execute its input.')
  await request({ op: 'terminal.stop', workspaceId, terminalId: terminal.id })
  assert.equal((await poll()).status.terminals.find(value => value.id === terminal.id)?.exit?.stopped, true)
  results.push({ terminalInput: true, terminalResize: true, terminalStop: true })
  console.log(JSON.stringify(results.at(-1)))
  await writeFile(join(root, 'input.py'), 'value = input("value: ")\nprint("GOT=" + value)\n')
  const inputRun = await request({
    op: 'run.start',
    workspaceId,
    configuration: { name: 'input', language: 'python', program: 'input.py', terminal: true },
  })
  await until(
    async () =>
      (await poll()).events.some(event => event.kind === 'output' && event.operationId === inputRun.id && event.text.includes('value: '))
        ? true
        : undefined,
    'The program did not request stdin.',
  )
  await request({ op: 'run.input', workspaceId, runId: inputRun.id, data: 'hello\n' })
  await until(
    async () => (await poll()).status.runs.find(value => value.id === inputRun.id && value.phase === 'exited'),
    'The program did not exit after stdin.',
  )
  assert.match(
    (await poll()).events.flatMap(event => (event.kind === 'output' && event.operationId === inputRun.id ? [event.text] : [])).join(''),
    /GOT=hello/,
  )
  results.push({ runInput: true })
  console.log(JSON.stringify(results.at(-1)))
  await writeFile(
    join(root, 'input.c'),
    '#include <stdio.h>\nint main(void) {\n  int x = 0;\n  printf("value: ");\n  fflush(stdout);\n  scanf("%d", &x);\n  printf("GOT=%d\\n", x);\n  return 0;\n}\n',
  )
  const inputDebug = await request({
    op: 'debug.start',
    workspaceId,
    configuration: { name: 'native-input', language: 'c', program: 'input.c', terminal: true },
    breakpoints: [{ path: 'input.c', lines: [7] }],
  })
  await until(
    async () =>
      (await poll()).events.some(event => event.kind === 'output' && event.operationId === inputDebug.id && event.text.includes('value: '))
        ? true
        : undefined,
    'The native debugger did not request terminal input.',
  )
  await request({ op: 'debug.input', workspaceId, debugId: inputDebug.id, data: '42\n' })
  const nativeInputFrame = await frame(inputDebug.id)
  assert.equal(
    (
      await request({
        op: 'debug.evaluate',
        workspaceId,
        debugId: inputDebug.id,
        expression: 'x',
        context: 'watch',
        frameId: nativeInputFrame.top.id,
      })
    ).result,
    '42',
  )
  await request({ op: 'debug.control', workspaceId, debugId: inputDebug.id, action: 'continue', threadId: nativeInputFrame.threadId })
  await until(
    async () => (await poll()).status.debugSessions.find(value => value.id === inputDebug.id && value.phase === 'terminated'),
    'The native debugger did not exit after terminal input.',
  )
  assert.match(
    (await poll()).events.flatMap(event => (event.kind === 'output' && event.operationId === inputDebug.id ? [event.text] : [])).join(''),
    /GOT=42/,
  )
  results.push({ nativeDebugInput: true, watch: '42' })
  console.log(JSON.stringify(results.at(-1)))
  assert.deepEqual(failures, [])
} finally {
  try {
    await service.dispose()
    for (const handle of handles)
      assert.equal(await handle.waitForExit(AbortSignal.timeout(3000)), true, 'An IDE process range survived service disposal.')
    for (const terminal of terminals)
      await Promise.race([
        terminal.done,
        delay(3000, undefined, { ref: false }).then(() => {
          throw new Error('An IDE terminal survived service disposal.')
        }),
      ])
  } finally {
    await context.fiber.dispose()
    await rm(root, { recursive: true, force: true })
  }
}
console.log(JSON.stringify({ ok: true, results }))
