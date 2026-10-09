/** Real CMake compiler-selection check using an owned wrapper around the installed C++ compiler. */
import assert from 'node:assert/strict'
import { chmod, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { createHash } from 'node:crypto'
import { Context } from '@deepseek-ai/cordis'
import LocalSubprocessRuntime from '@deepseek-ai/dsh-subprocess-local'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import type { IdeExecutionPoll, IdeRunSnapshot } from '@deepseek-ai/dsh-client-ui-rainy/ide-execution-protocol'
import { createIdeExecutionService } from '../src/ide-execution.ts'
import { ideExecutionLimitsSchema } from '../src/ide-execution-schema.ts'
import { getIdeToolPaths } from '../src/ide-tools.ts'

const root = await mkdtemp(join(tmpdir(), 'rainy-cmake-compiler-'))
const context = new Context()
await context.plugin(LocalSubprocessRuntime)
const workspaceId = WorkspaceId('cmake-compiler-smoke')
const failures: string[] = []
const service = createIdeExecutionService({
  assertUsable: () => {},
  subprocess: context.subprocess,
  resources: getIdeToolPaths(),
  limits: ideExecutionLimitsSchema.parse({}),
  resolveWorkspace: async (id) => {
    assert.equal(id, workspaceId)
    return { workspaceId, root }
  },
  reportError: (error) => {
    failures.push(String(error))
  },
})
try {
  const compiler = await context.subprocess.resolveExecutable('g++')
  const wrapper = join(root, 'chosen compiler')
  const quotedCompiler = "'" + compiler.replaceAll("'", "'\\''") + "'"
  const wrapperText = '#!/bin/sh\nexec ' + quotedCompiler + ' "$@"\n'
  await writeFile(wrapper, wrapperText)
  await chmod(wrapper, 0o700)
  await writeFile(join(root, 'main.cpp'), '#include <iostream>\nint main() { std::cout << 42 << std::endl; }\n')
  await writeFile(
    join(root, 'CMakeLists.txt'),
    'cmake_minimum_required(VERSION 3.20)\nproject(CompilerSelection CXX)\nadd_executable(main main.cpp)\n',
  )
  const started = (await service.handle({
    op: 'run.start',
    workspaceId,
    configuration: {
      name: 'chosen compiler',
      language: 'cpp',
      program: 'main.cpp',
      executable: wrapper,
      terminal: false,
      build: { kind: 'cmake', buildDirectory: 'build', executable: 'main', target: 'main' },
    },
  })) as IdeRunSnapshot
  const deadline = Date.now() + 60000
  let observation: IdeExecutionPoll | undefined
  for (;;) {
    observation = (await service.handle({ op: 'execution.poll', workspaceId, cursor: 0 })) as IdeExecutionPoll
    const current = observation.status.runs.find(run => run.id === started.id)
    if (current?.phase === 'failed') throw new Error(JSON.stringify(observation))
    if (current?.phase === 'exited') {
      assert.equal(current.exit?.exitCode, 0)
      break
    }
    if (Date.now() > deadline) throw new Error('The compiler-selection fixture exceeded its deadline.')
    await delay(25)
  }
  const cache = await readFile(join(root, 'build/CMakeCache.txt'), 'utf8')
  const selected = cache.split('\n').find(line => /^CMAKE_CXX_COMPILER:[^=]+=/.test(line))
  assert(selected?.endsWith('=' + wrapper), selected)
  assert(started.spec.build[0]?.argv.includes('-DCMAKE_CXX_COMPILER=' + wrapper))
  const output = observation.events
    .flatMap(event => (event.kind === 'output' && event.operationId === started.id ? [event.text] : []))
    .join('')
  assert.match(output, /(?:^|\n)42\r?\n/)
  assert.deepEqual(failures, [])
  await service.dispose()
  console.log(
    JSON.stringify({
      ok: true,
      language: 'cpp',
      compiler: wrapper,
      underlyingCompiler: compiler,
      compilerCacheEntry: selected,
      compilerArgument: '-DCMAKE_CXX_COMPILER=' + wrapper,
      output: '42',
      exitCode: 0,
      wrapperSha256: createHash('sha256').update(wrapperText).digest('hex'),
      cmakeCacheSha256: createHash('sha256').update(cache).digest('hex'),
    }),
  )
} finally {
  await service.dispose()
  await context.fiber.dispose()
  await rm(root, { recursive: true, force: true })
}
