/** Real native runtime, compiler and debugger acceptance using application-owned component staging. */
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { delimiter, join, resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { Context } from '@deepseek-ai/cordis'
import LocalSubprocessRuntime from '@deepseek-ai/dsh-subprocess-local'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type { IdeExecutionRequest, IdeExecutionResponse, IdeExecutionLanguage } from '@deepseek-ai/dsh-client-ui-rainy/ide-execution-protocol'
import { createIdeExecutionService } from '../src/ide-execution.ts'
import { ideExecutionLimitsSchema } from '../src/ide-execution-schema.ts'
import { getIdeToolPaths } from '../src/ide-tools.ts'

assert.equal(process.platform, 'win32')
const components = resolve(process.env.RAINY_SMOKE_COMPONENT_ROOT ?? 'apps/rainy-desktop/runtime/component-stage')
const resources = resolve(process.env.RAINY_SMOKE_IDE_ROOT ?? 'apps/rainy-desktop/runtime/windows-host/app/resources/ide')
const root = await mkdtemp(join(tmpdir(), 'rainy-native-runtime-'))
const workspaceId = WorkspaceId('native-runtime-smoke')
const context = new Context()
await context.plugin(LocalSubprocessRuntime)
const environment = {
  PATH: [join(components, 'windows-basic/python'), join(components, 'windows-basic/python/Scripts'), join(components, 'windows-basic/node'),
    join(components, 'windows-cpp/cpp/bin'), join(components, 'windows-cpp/codelldb/extension/adapter'),
    join(components, 'windows-cpp/cmake/bin'), join(components, 'windows-cpp/ninja'), process.env.PATH ?? ''].join(delimiter),
  PYTHONDONTWRITEBYTECODE: '1',
  CMAKE_GENERATOR: 'Ninja',
}
const errors: string[] = []
const service = createIdeExecutionService({
  assertUsable: () => {},
  subprocess: context.subprocess,
  resources: { ...getIdeToolPaths(), resourceRoot: resources },
  resolveWorkspace: async () => ({ workspaceId, root }),
  resolveEnvironment: () => ({ environment, executables: {
    python: join(components, 'windows-basic/python/python.exe'), node: join(components, 'windows-basic/node/node.exe'),
    c: join(components, 'windows-cpp/cpp/bin/clang.exe'), cpp: join(components, 'windows-cpp/cpp/bin/clang++.exe'),
    php: join(components, 'windows-basic/php/php.exe'),
  } }),
  limits: ideExecutionLimitsSchema.parse({}),
  reportError: (error) => { errors.push(String(error)) },
})
async function request<T extends IdeExecutionRequest>(value: T): Promise<IdeExecutionResponse<T>> {
  return await service.handle(value) as IdeExecutionResponse<T>
}
const poll = () => request({ op: 'execution.poll', workspaceId, cursor: 0 })
async function until<T>(read: () => Promise<T | undefined>): Promise<T> {
  const deadline = Date.now() + 60000
  while (Date.now() < deadline) {
    const result = await read()
    if (result !== undefined) return result
    await delay(25)
  }
  throw new Error(`Native runtime acceptance timed out: ${JSON.stringify(await poll())}`)
}
const cases: { language: IdeExecutionLanguage; file: string; source: string; line?: number }[] = [
  { language: 'python', file: 'main.py', source: 'x = 41\nx += 1\nprint(x)\n', line: 2 },
  { language: 'javascript', file: 'main.js', source: 'const x = 41;\nconst y = x + 1;\nconsole.log(y);\n', line: 2 },
  { language: 'c', file: 'main.c', source: '#include <stdio.h>\nint main(void) {\n  int x = 41;\n  x += 1;\n  printf("%d\\n", x);\n  return 0;\n}\n', line: 4 },
  { language: 'cpp', file: 'main.cpp', source: '#include <iostream>\nint main() {\n  int x = 41;\n  x += 1;\n  std::cout << x << std::endl;\n  return 0;\n}\n', line: 4 },
  { language: 'php', file: 'main.php', source: '<?php echo 42, "\\n";\n' },
]
try {
  for (const item of process.env.RAINY_SMOKE_CMAKE_ONLY === '1' ? [] : cases) {
    await writeFile(join(root, item.file), item.source)
    const configuration = { language: item.language, name: item.language, program: item.file, terminal: false }
    const run = await request({ op: 'run.start', workspaceId, configuration })
    const exit = await until(async () => {
      const state = (await poll()).status.runs.find(value => value.id === run.id)
      if (state?.phase === 'failed') throw new Error(JSON.stringify(await poll()))
      return state?.phase === 'exited' ? state : undefined
    })
    assert.equal(exit.exit?.exitCode, 0)
    if (item.line === undefined) { console.log(JSON.stringify({ language: item.language, run: true })); continue }
    const debug = await request({ op: 'debug.start', workspaceId, configuration: { ...configuration, terminal: process.env.RAINY_SMOKE_TERMINAL === '1' }, breakpoints: [{ path: item.file, lines: [item.line] }] })
    const stopped = await until(async () => {
      const state = (await poll()).status.debugSessions.find(value => value.id === debug.id)
      if (state?.phase === 'failed' || state?.phase === 'terminated') throw new Error(JSON.stringify(await poll()))
      return state?.phase === 'paused' && state.breakpoints.some(value => value.verified) ? state : undefined
    })
    assert(stopped.breakpoints.some(value => value.verified))
    const threads = await request({ op: 'debug.threads', workspaceId, debugId: debug.id })
    const threadId = stopped.threadId ?? threads[0].id
    const frames = await request({ op: 'debug.stack', workspaceId, debugId: debug.id, threadId })
    assert.equal(frames[0]?.line, item.line)
    const watch = await request({ op: 'debug.evaluate', workspaceId, debugId: debug.id, expression: 'x', frameId: frames[0].id, context: 'watch' })
    assert.equal(watch.result, '41')
    await request({ op: 'debug.control', workspaceId, debugId: debug.id, action: 'continue', threadId })
    const complete = await until(async () => {
      const state = (await poll()).status.debugSessions.find(value => value.id === debug.id)
      if (state?.phase === 'failed') throw new Error(JSON.stringify(await poll()))
      return state?.phase === 'terminated' ? state : undefined
    })
    if (item.language !== 'javascript') assert.equal(complete.exit?.exitCode, 0)
    assert.match((await poll()).events.flatMap(event => event.kind === 'output' && event.operationId === debug.id ? [event.text] : []).join(''), /42/)
    console.log(JSON.stringify({ language: item.language, run: true, breakpoint: item.line, watch: watch.result, cleanup: complete.phase }))
  }
  await writeFile(join(root, 'cmake.c'), '#include <stdio.h>\nint main(void){puts("42");return 0;}\n')
  await writeFile(join(root, 'CMakeLists.txt'), 'cmake_minimum_required(VERSION 3.20)\nproject(RainySmoke C)\nadd_executable(hello cmake.c)\n')
  await mkdir(join(root, 'build'))
  const cmake = await request({ op: 'run.start', workspaceId, configuration: { name: 'cmake', language: 'c', program: 'cmake.c', terminal: false,
    build: { kind: 'cmake', buildDirectory: 'build', target: 'hello', executable: 'hello.exe' } } })
  const cmakeExit = await until(async () => {
    const state = (await poll()).status.runs.find(value => value.id === cmake.id)
    if (state?.phase === 'failed') throw new Error(JSON.stringify(await poll()))
    return state?.phase === 'exited' ? state : undefined
  })
  assert.equal(cmakeExit.exit?.exitCode, 0)
  assert.match(await readFile(join(root, 'build/compile_commands.json'), 'utf8'), /cmake\.c/)
  console.log(JSON.stringify({ cmake: true, compiled: true, exit: cmakeExit.exit?.exitCode }))
  assert.deepEqual(errors, [])
} finally {
  await service.dispose()
  await context.fiber.dispose()
  await rm(root, { recursive: true, force: true })
}
