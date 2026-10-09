/** Import the published Linux archive into a private home and boot its real named profile. */
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, posix, resolve } from 'node:path'
import { z } from 'zod'
import { WslHostTransport } from '../src/transport.ts'
import { runtimeResolutionProbe, runtimeResolutionSchema, useExternalWorkingDirectory } from './fixtures/isolated-runtime.ts'

const run = promisify(execFile)
const app = resolve(import.meta.dirname, '..')
const resources = process.env.RAINY_SMOKE_RESOURCES_ROOT
const archiveRoot = resources ? resolve(resources) : join(app, 'runtime')
const installerRoot = resources ? resolve(resources) : join(app, 'scripts')
const distro = process.env.RAINY_SMOKE_WSL_DISTRIBUTION ?? 'Ubuntu'
const directory = await mkdtemp(join(tmpdir(), 'rainy-wsl-host-'))
const previousCwd = useExternalWorkingDirectory(directory)
const linux = async (path: string): Promise<string> => (await run('wsl.exe', ['-d', distro, '--exec', 'wslpath', '-u', path], { windowsHide: true, timeout: 15000 })).stdout.trim()
const home = (await run('wsl.exe', ['-d', distro, '--exec', 'mktemp', '-d', '/var/tmp/rainy-host-smoke-XXXXXXXX'], { windowsHide: true, timeout: 15000 })).stdout.trim()
if (!/^\/var\/tmp\/rainy-host-smoke-[A-Za-z0-9]+$/u.test(home)) throw new Error('The WSL smoke returned an unexpected private home.')
const diagnostics: string[] = []
let transport: WslHostTransport | undefined
try {
  const installed = z.object({ node: z.string(), host: z.string() }).strict().parse(JSON.parse((await run('wsl.exe', ['-d', distro, '--exec', 'env', `HOME=${home}`,
    'python3', await linux(join(installerRoot, 'install-runtime.py')), await linux(join(archiveRoot, 'linux-runtime.tar.gz')), await linux(join(archiveRoot, 'linux-runtime.json'))],
  { windowsHide: true, timeout: 300000, maxBuffer: 1024 * 1024 })).stdout))
  const inheritedHooks = (await run('wsl.exe', ['-d', distro, '--exec', 'python3', '-c', 'import os,json; print(json.dumps([k for k in os.environ if k in ("NODE_PATH","NODE_OPTIONS") or k.startswith("DSH_")]))'], { windowsHide: true })).stdout
  assert.deepEqual(JSON.parse(inheritedHooks), [], 'The external WSL fixture inherited Node or DSH overrides.')
  const runtime = posix.dirname(posix.dirname(posix.dirname(installed.host)))
  const resolution = runtimeResolutionSchema.parse(JSON.parse((await run('wsl.exe', ['-d', distro, '--exec', installed.node,
    '--input-type=module', '-e', runtimeResolutionProbe, runtime], { windowsHide: true, timeout: 120000, maxBuffer: 1024 * 1024 })).stdout))
  transport = new WslHostTransport({ distro, ...installed, entry: installed.host,
    environment: { HOME: home, RAINY_HOME: `${home}/state`, RAINY_CARRIER_STATE_ROOT: `${home}/carrier`, RAINY_EXECUTION_TARGET_ID: 'wsl:production-smoke' },
    onDiagnostic: (text) => { diagnostics.push(text) },
  })
  const ready = await transport.start()
  const actualCwd = (await run('wsl.exe', ['-d', distro, '--exec', 'readlink', `/proc/${ready.pid}/cwd`], { windowsHide: true })).stdout.trim()
  assert.equal(actualCwd, await linux(directory))
  assert.equal(ready.home, `${home}/state`)
  assert.equal((await transport.inspectActivity()).active, false)
  assert.equal(await transport.inspectProject(), null)
  const launch = await fetch(ready.url, { redirect: 'manual' })
  assert.equal(launch.status, 303)
  const cookie = launch.headers.get('set-cookie')?.split(';')[0]
  assert(cookie)
  const response = await fetch(new URL('/', ready.url), { headers: { cookie } })
  assert.equal(response.status, 200)
  const projectPath = `${home}/project`
  await run('wsl.exe', ['-d', distro, '--exec', 'mkdir', projectPath], { windowsHide: true, timeout: 15000 })
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
  const report = { ok: true, target: 'linux', distro, protocol: ready.protocol, page: response.status, idle: true,
    releaseArchive: true, packagedResources: Boolean(resources), freshSelectionEmpty: true, selectedProjectResolved: true,
    repositoryIndependent: true, actualCwd, activationRequired: false, inheritedResolverOverrides: [], resolution }
  const evidencePath = process.env.RAINY_SMOKE_EVIDENCE_DIRECTORY ? join(process.env.RAINY_SMOKE_EVIDENCE_DIRECTORY, 'wsl-host.json') : join(app, 'validation/production-wsl-host-external.json')
  await mkdir(dirname(evidencePath), { recursive: true })
  await writeFile(evidencePath, JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify(report))
} catch (error) {
  console.error(diagnostics.join('\n'))
  throw error
} finally {
  await transport?.stop()
  process.chdir(previousCwd)
  await run('wsl.exe', ['-d', distro, '--cd', '/var/tmp', '--exec', 'python3', '-c', 'from pathlib import Path; import shutil,sys; p=Path(sys.argv[1]); assert p.parent==Path("/var/tmp") and p.name.startswith("rainy-host-smoke-") and p.resolve()==p; shutil.rmtree(p)', home], { cwd: tmpdir(), windowsHide: true, timeout: 300000 })
  await rm(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
}
