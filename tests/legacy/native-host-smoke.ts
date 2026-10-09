/** A fresh named Rainy profile on the materialized Windows runtime with isolated state and no activation input. */
import assert from 'node:assert/strict'
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, resolve } from 'node:path'
import { WindowsHostTransport } from '../src/transport.ts'
import { z } from 'zod'
import { copyExternalRuntime, inspectWindowsRuntime, useExternalWorkingDirectory } from './fixtures/isolated-runtime.ts'

const directory = await mkdtemp(join(tmpdir(), 'rainy-native-host-'))
const sourceRuntime = resolve(process.env.RAINY_SMOKE_HOST_ROOT ?? 'apps/rainy-desktop/runtime/windows-host')
const previousCwd = useExternalWorkingDirectory(directory)
const runtime = await copyExternalRuntime(sourceRuntime, directory)
const diagnostics: string[] = []
const transport = new WindowsHostTransport({
  cwd: runtime, node: join(runtime, 'node/node.exe'), entry: join(runtime, 'app/lib/host.js'),
  environment: { RAINY_HOME: join(directory, 'home'), RAINY_CARRIER_STATE_ROOT: join(directory, 'carrier'),
    RAINY_EXECUTION_TARGET_ID: 'windows-local', RAINY_PWSH_PATH: join(runtime, 'pwsh/pwsh.exe') },
  onDiagnostic: (text) => { diagnostics.push(text) },
})
try {
  const resolution = await inspectWindowsRuntime(runtime, directory)
  const ready = await transport.start()
  assert.equal(ready.home, join(directory, 'home'))
  assert.equal((await transport.inspectActivity()).active, false)
  assert.equal(await transport.inspectProject(), null)
  const launch = await fetch(ready.url, { redirect: 'manual' })
  assert.equal(launch.status, 303)
  const cookie = launch.headers.get('set-cookie')?.split(';')[0]
  assert(cookie)
  const response = await fetch(new URL('/', ready.url), { headers: { cookie } })
  assert.equal(response.status, 200)
  const projectPath = join(directory, 'project')
  await mkdir(projectPath)
  const ide = async (request: object): Promise<unknown> => {
    const result = await fetch(new URL('/rainy/ide', ready.url), { method: 'POST', headers: { cookie, 'Content-Type': 'application/json' }, body: JSON.stringify(request) })
    assert.equal(result.status, 200)
    return await result.json()
  }
  const opened = z.object({ ok: z.literal(true), value: z.object({ workspaceId: z.string() }).loose() }).parse(await ide({ op: 'workspaces.open', path: projectPath }))
  await ide({ op: 'state.selection.save', workspaceId: opened.value.workspaceId })
  const selected = await transport.inspectProject()
  assert.equal(selected?.workspaceId, opened.value.workspaceId)
  assert.equal(selected?.roots[0]?.path, projectPath)
  assert.equal((await transport.inspectActivity('freeze')).active, false)
  await transport.inspectActivity('resume')
  const report = { ok: true, target: 'windows-local', protocol: ready.protocol, page: response.status, idle: true,
    freshSelectionEmpty: true, selectedProjectResolved: true, repositoryIndependent: true, physicalRuntimeCopy: true,
    driverCwd: directory, activationRequired: false, resolution }
  const evidencePath = process.env.RAINY_SMOKE_EVIDENCE_DIRECTORY ? join(process.env.RAINY_SMOKE_EVIDENCE_DIRECTORY, 'native-host.json') : resolve(import.meta.dirname, '../validation/production-native-host-external.json')
  await mkdir(dirname(evidencePath), { recursive: true })
  await writeFile(evidencePath, JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify(report))
} catch (error) {
  console.error(diagnostics.join('\n'))
  throw error
} finally {
  await transport.stop()
  process.chdir(previousCwd)
  await rm(directory, { recursive: true, force: true })
}
