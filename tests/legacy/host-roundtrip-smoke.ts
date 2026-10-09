/** Production Windows/WSL project roundtrip with shared identity and independent unsaved recovery rows. */
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join, posix, resolve } from 'node:path'
import { z } from 'zod'
import { createConnection } from 'node:net'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import { WindowsHostTransport, WslHostTransport } from '../src/transport.ts'
import type { HostTransport, HostProjectSnapshot } from '../src/transport.ts'
import { ideStateDataSchema } from '../src/ide-state.ts'
import { componentDigest } from '../src/environment-components.ts'
import { copyExternalRuntime, inspectWindowsRuntime, runtimeResolutionProbe, runtimeResolutionSchema, useExternalWorkingDirectory } from './fixtures/isolated-runtime.ts'

const execute = promisify(execFile)
const app = resolve(import.meta.dirname, '..')
const sourceRuntime = resolve(process.env.RAINY_SMOKE_HOST_ROOT ?? join(app, 'runtime/windows-host'))
const resources = process.env.RAINY_SMOKE_RESOURCES_ROOT
const archiveRoot = resources ? resolve(resources) : join(app, 'runtime')
const installerRoot = resources ? resolve(resources) : join(app, 'scripts')
const distro = process.env.RAINY_SMOKE_WSL_DISTRIBUTION ?? 'Ubuntu'
const directory = await mkdtemp(join(tmpdir(), 'rainy-roundtrip-'))
const previousCwd = useExternalWorkingDirectory(directory)
const runtime = await copyExternalRuntime(sourceRuntime, directory)
const carrier = join(directory, 'carrier')
const project = join(directory, 'project')
const attached = join(directory, 'attached')
const windowsOther = join(directory, 'other')
for (const path of [carrier, project, attached, windowsOther]) await mkdir(path)
const wsl = async (args: string[], timeout = 300000): Promise<string> => (await execute('wsl.exe', ['-d', distro, '--exec', ...args], { windowsHide: true, timeout, maxBuffer: 1024 * 1024 })).stdout.trim()
const linuxPath = (path: string): Promise<string> => wsl(['wslpath', '-u', path], 15000)
const linuxHome = await wsl(['mktemp', '-d', '/var/tmp/rainy-roundtrip-XXXXXXXX'], 15000)
if (!/^\/var\/tmp\/rainy-roundtrip-[A-Za-z0-9]+$/u.test(linuxHome)) throw new Error('The roundtrip returned an unexpected private Linux home.')
const linuxTarget = 'wsl:production-roundtrip'
const diagnostics: string[] = []
const observations: object[] = []
let phase = 'fixture preparation'
let lastReady: { label: string; pid: number; port: number } | undefined
function observe(event: string, details: object = {}): void {
  observations.push({ at: new Date().toISOString(), phase, event, ...details })
}
function diagnostic(text: string): void {
  diagnostics.push(text.slice(-4000))
  if (diagnostics.length > 80) diagnostics.shift()
}
function windowsConnection(port: number): Promise<object> {
  return new Promise((accept) => {
    const socket = createConnection({ host: '127.0.0.1', port })
    const finish = (result: object) => { socket.destroy(); accept({ at: new Date().toISOString(), ...result }) }
    socket.once('connect', () => { finish({ connected: true }) })
    socket.once('error', (error: NodeJS.ErrnoException) => { finish({ connected: false, code: error.code }) })
    socket.setTimeout(1500, () => { finish({ connected: false, timedOut: true }) })
  })
}
const linuxWorkingDirectories: string[] = []
let active: HostTransport | undefined
const workspaceSchema = z.object({ workspaceId: z.string().transform(WorkspaceId) }).loose()
const stateSchema = z.object({ revision: z.number(), data: ideStateDataSchema }).loose()
async function start(transport: HostTransport, label: string) {
  active = transport
  lastReady = undefined
  phase = `${label}: waiting for ready`
  const ready = await transport.start()
  lastReady = { label, pid: ready.pid, port: Number(new URL(ready.url).port) }
  observe('host-ready', lastReady)
  if (transport instanceof WslHostTransport) {
    const cwd = await wsl(['readlink', `/proc/${ready.pid}/cwd`], 15000)
    assert.equal(cwd, await linuxPath(directory))
    linuxWorkingDirectories.push(cwd)
  }
  phase = `${label}: launch HTTP request`
  const launch = await fetch(ready.url, { redirect: 'manual' })
  assert.equal(launch.status, 303)
  const cookie = launch.headers.get('set-cookie')?.split(';')[0]
  assert(cookie)
  return async (request: object): Promise<unknown> => {
    phase = `${label}: ${'op' in request && typeof request.op === 'string' ? request.op : 'IDE request'}`
    const response = await fetch(new URL('/rainy/ide', ready.url), { method: 'POST', headers: { cookie, 'Content-Type': 'application/json' }, body: JSON.stringify(request) })
    const result: unknown = await response.json()
    assert.equal(response.status, 200, JSON.stringify(result))
    return z.object({ ok: z.literal(true), value: z.unknown() }).parse(result).value
  }
}
const windows = (pending?: HostProjectSnapshot): WindowsHostTransport => new WindowsHostTransport({ cwd: runtime, node: join(runtime, 'node/node.exe'), entry: join(runtime, 'app/lib/host.js'),
  environment: { RAINY_HOME: join(directory, 'windows-home'), RAINY_CARRIER_STATE_ROOT: carrier, RAINY_EXECUTION_TARGET_ID: 'windows-local',
    RAINY_PWSH_PATH: join(runtime, 'pwsh/pwsh.exe'), ...(pending ? { RAINY_PENDING_PROJECT_ID: pending.projectId, RAINY_PENDING_PROJECT_PATH: project,
      RAINY_PENDING_PROJECT_ROOTS: JSON.stringify(pending.roots.filter(root => !root.primary)
        .map(root => ({ rootId: root.rootId, title: root.title, path: attached }))) } : {}) },
  onDiagnostic: diagnostic,
  onExit: (code) => { observe('host-exit', { label: pending ? 'windows-return' : 'windows-initial', code }) },
})
async function saveOther(api: (request: object) => Promise<unknown>, path: string, content: string): Promise<WorkspaceId> {
  const workspace = workspaceSchema.parse(await api({ op: 'workspaces.open', path }))
  const file = z.object({ version: z.string() }).loose().parse(await api({ op: 'files.create', workspaceId: workspace.workspaceId, path: 'keep.txt', content: 'saved on disk\n' }))
  const state = stateSchema.parse(await api({ op: 'state.read', workspaceId: workspace.workspaceId }))
  const data = ideStateDataSchema.parse({ ...state.data, tabs: [{ path: 'keep.txt', kind: 'file' }], activePath: 'keep.txt',
    buffers: [{ path: 'keep.txt', content, baseVersion: file.version, bom: false, eol: 'lf' }] })
  await api({ op: 'state.save', workspaceId: workspace.workspaceId, baseRevision: state.revision, data })
  await api({ op: 'state.selection.save', workspaceId: workspace.workspaceId })
  return workspace.workspaceId
}
async function checkSelection(api: (request: object) => Promise<unknown>, expected: WorkspaceId): Promise<void> {
  assert.equal(z.object({ workspaceId: z.string() }).loose().parse(await api({ op: 'state.selection.read' })).workspaceId, expected)
}
async function checkDraft(api: (request: object) => Promise<unknown>, id: WorkspaceId, text: string): Promise<void> {
  assert.equal(stateSchema.parse(await api({ op: 'state.read', workspaceId: id })).data.buffers[0]?.content, text)
}
try {
  const windowsResolution = await inspectWindowsRuntime(runtime, directory)
  const win1 = windows()
  const winApi1 = await start(win1, 'windows-initial')
  const winProject = workspaceSchema.parse(await winApi1({ op: 'workspaces.open', path: project }))
  await winApi1({ op: 'workspaces.attach', workspaceId: winProject.workspaceId, path: attached })
  const winOtherId = await saveOther(winApi1, windowsOther, 'Windows dirty buffer\n')
  const outgoing = await win1.inspectProject(winProject.workspaceId)
  assert(outgoing)
  assert.equal(outgoing.roots.length, 2)
  await win1.stop()
  active = undefined

  const installed = z.object({ node: z.string(), host: z.string() }).strict().parse(JSON.parse(await wsl(['env', `HOME=${linuxHome}`, 'python3',
    await linuxPath(join(installerRoot, 'install-runtime.py')), await linuxPath(join(archiveRoot, 'linux-runtime.tar.gz')), await linuxPath(join(archiveRoot, 'linux-runtime.json'))])))
  const inheritedHooks = await wsl(['python3', '-c', 'import os,json; print(json.dumps([k for k in os.environ if k in ("NODE_PATH","NODE_OPTIONS") or k.startswith("DSH_")]))'])
  assert.deepEqual(JSON.parse(inheritedHooks), [], 'The roundtrip inherited Node or DSH overrides.')
  const linuxRuntime = posix.dirname(posix.dirname(posix.dirname(installed.host)))
  const linuxResolution = runtimeResolutionSchema.parse(JSON.parse(await wsl([installed.node, '--input-type=module', '-e', runtimeResolutionProbe, linuxRuntime])))
  const mappedCarrier = await linuxPath(carrier)
  const mappedProject = await linuxPath(project)
  const mappedAttached = await linuxPath(attached)
  const linuxEnvironment = { HOME: linuxHome, RAINY_HOME: `${linuxHome}/state`, RAINY_CARRIER_STATE_ROOT: mappedCarrier, RAINY_EXECUTION_TARGET_ID: linuxTarget }
  const linux = (pending = false): WslHostTransport => new WslHostTransport({ distro, node: installed.node, entry: installed.host,
    environment: { ...linuxEnvironment, ...(pending ? {
      RAINY_PENDING_PROJECT_ID: outgoing.projectId, RAINY_PENDING_PROJECT_PATH: mappedProject,
      RAINY_PENDING_PROJECT_ROOTS: JSON.stringify(outgoing.roots.filter(root => !root.primary)
        .map(root => ({ rootId: root.rootId, title: root.title, path: mappedAttached }))) } : {}) },
    onDiagnostic: diagnostic,
    onExit: (code) => { observe('host-exit', { label: pending ? 'wsl-incoming' : 'wsl-seed', code }) },
  })
  await wsl(['mkdir', `${linuxHome}/other`], 15000)
  const linuxSeed = linux()
  const linuxSeedApi = await start(linuxSeed, 'wsl-seed')
  const linuxOtherId = await saveOther(linuxSeedApi, `${linuxHome}/other`, 'Linux dirty buffer\n')
  await linuxSeed.stop()
  active = undefined

  const linuxHost = linux(true)
  const linuxApi = await start(linuxHost, 'wsl-incoming')
  const incoming = await linuxHost.inspectProject()
  assert(incoming)
  assert.equal(incoming.projectId, outgoing.projectId)
  assert.notEqual(incoming.workspaceId, outgoing.workspaceId)
  assert.equal(incoming.roots.find(root => !root.primary)?.rootId, outgoing.roots.find(root => !root.primary)?.rootId)
  await checkSelection(linuxApi, incoming.workspaceId)
  await checkDraft(linuxApi, linuxOtherId, 'Linux dirty buffer\n')
  let catalog = z.object({ projects: z.array(z.object({ projectId: z.string(), activeTargetId: z.string() }).loose()) }).loose().parse(JSON.parse(await readFile(join(carrier, 'projects.json'), 'utf8')))
  assert.equal(catalog.projects.find(value => value.projectId === outgoing.projectId)?.activeTargetId, linuxTarget)
  await linuxHost.stop()
  active = undefined

  const win2 = windows(incoming)
  const winApi2 = await start(win2, 'windows-return')
  const restored = await win2.inspectProject()
  assert(restored)
  assert.equal(restored.projectId, outgoing.projectId)
  assert.equal(restored.workspaceId, outgoing.workspaceId)
  assert.deepEqual(restored.roots, outgoing.roots)
  await checkSelection(winApi2, outgoing.workspaceId)
  await checkDraft(winApi2, winOtherId, 'Windows dirty buffer\n')
  catalog = z.object({ projects: z.array(z.object({ projectId: z.string(), activeTargetId: z.string() }).loose()) }).loose().parse(JSON.parse(await readFile(join(carrier, 'projects.json'), 'utf8')))
  assert.equal(catalog.projects.find(value => value.projectId === outgoing.projectId)?.activeTargetId, 'windows-local')
  const report = { ok: true, order: ['windows', 'wsl', 'windows'], projectIdentityRetained: true, attachedRootIdentityRetained: true,
    originalWindowsWorkspaceRetained: true, selectionRestoredInBothTargets: true, separateDraftsPreserved: true, activeTarget: 'windows-local',
    packagedResources: Boolean(resources), repositoryIndependent: true, physicalWindowsRuntimeCopy: true,
    driverCwd: directory, linuxWorkingDirectories, activationRequired: false,
    observations, windowsResolution, linuxResolution,
    windowsInventorySha256: await componentDigest(join(runtime, 'runtime.json')),
    linuxArchive: z.object({ sha256: z.string(), bytes: z.number(), node: z.string(), upstream: z.string() }).strict()
      .parse(JSON.parse(await readFile(join(archiveRoot, 'linux-runtime.json'), 'utf8'))) }
  const evidencePath = process.env.RAINY_SMOKE_EVIDENCE_DIRECTORY ? join(process.env.RAINY_SMOKE_EVIDENCE_DIRECTORY, 'host-roundtrip.json') : join(app, 'validation/production-host-roundtrip.json')
  await mkdir(dirname(evidencePath), { recursive: true })
  await writeFile(evidencePath, JSON.stringify(report, null, 2) + '\n')
  console.log(JSON.stringify(report))
} catch (error) {
  observe('test-failed', { errorType: error instanceof Error ? error.name : typeof error })
  if (lastReady && active instanceof WslHostTransport) {
    observe('windows-port-after-failure', await windowsConnection(lastReady.port))
    try { observe('control-after-failure', await active.inspectActivity()) }
    catch (controlError) { observe('control-failed', { errorType: controlError instanceof Error ? controlError.name : typeof controlError }) }
    const probe = 'import json,os,socket,sys; p="/proc/"+sys.argv[1]; s=socket.socket(); s.settimeout(1); r=s.connect_ex(("127.0.0.1",int(sys.argv[2]))); s.close(); print(json.dumps({"pidExists":os.path.exists(p),"executable":os.readlink(p+"/exe") if os.path.exists(p) else None,"linuxLoopbackConnectResult":r}))'
    try { observe('linux-port-after-failure', z.object({ pidExists: z.boolean(), executable: z.string().nullable(), linuxLoopbackConnectResult: z.number() })
      .strict().parse(JSON.parse(await wsl(['python3', '-c', probe, String(lastReady.pid), String(lastReady.port)], 15000)))) }
    catch (probeError) { observe('linux-probe-failed', { errorType: probeError instanceof Error ? probeError.name : typeof probeError }) }
    observe('windows-port-after-linux-observation', await windowsConnection(lastReady.port))
  }
  console.error(JSON.stringify({ phase, lastReady,  observations }))
  console.error(diagnostics.join('\n'))
  throw error
} finally {
  await active?.stop()
  process.chdir(previousCwd)
  await execute('wsl.exe', ['-d', distro, '--cd', '/var/tmp', '--exec', 'python3', '-c', 'from pathlib import Path; import shutil,sys; p=Path(sys.argv[1]); assert p.parent==Path("/var/tmp") and p.name.startswith("rainy-roundtrip-") and p.resolve()==p; shutil.rmtree(p)', linuxHome], { cwd: tmpdir(), windowsHide: true, timeout: 300000 })
  await rm(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 })
}
