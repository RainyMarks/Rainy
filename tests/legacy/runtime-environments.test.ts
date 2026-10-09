/** Isolated runtime selection, platform rejection and public project migration checks. */
import { afterEach, describe, expect, it } from 'vitest'
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { dirname, isAbsolute, join, relative } from 'node:path'
import { tmpdir } from 'node:os'
import { WorkspaceId } from '@deepseek-ai/dsh-workspace'
import { createProjectRegistry, ExecutionTargetId } from '../src/project-registry.ts'
import { RuntimeEnvironments, probeRuntimeCandidate } from '../src/runtime-environments.ts'
import { savedExecutionTarget, saveExecutionTarget, readDesktopPreferences, WINDOWS_TARGET } from '../src/execution-targets.ts'
import { resolveIdeRun } from '../src/ide-execution-resolve.ts'

const temporary: string[] = []
async function directory(): Promise<string> { const value = await mkdtemp(join(tmpdir(), 'rainy-runtime-test-')); temporary.push(value); return value }
afterEach(async () => {
  for (const path of temporary.splice(0)) {
    const child = relative(tmpdir(), path)
    if (!child || child.startsWith('..') || isAbsolute(child)) throw new Error('Runtime test cleanup escaped its temporary root.')
    await rm(path, { recursive: true, force: true })
  }
})

describe('runtime selection', () => {
  it.skipIf(!process.env.RAINY_RUNTIME_TEST_PYTHON)('does not write bytecode while inspecting existing Python packages', async () => {
    const root = await directory()
    await writeFile(join(root, 'rainy_bytecode_fixture.py'), 'answer = 42\n')
    const path = process.env.RAINY_RUNTIME_TEST_PYTHON!
    const result = await probeRuntimeCandidate({ targetId: 'bytecode-test', platform: process.platform === 'win32' ? 'windows' : 'linux',
      language: 'python', path, source: 'manual', timeoutMs: 20000, run: async (command) => {
        const args = [...command.arguments]
        const index = args.indexOf('-c') + 1
        args[index] = `import sys; sys.path.insert(0, ${JSON.stringify(root)}); import rainy_bytecode_fixture; assert rainy_bytecode_fixture.answer == 42\n${args[index]}`
        return (await promisify(execFile)(command.executable, args, { timeout: command.timeoutMs, windowsHide: true,
          env: { ...process.env, PYTHONDONTWRITEBYTECODE: '1', CUDA_VISIBLE_DEVICES: '' } })).stdout
      } })
    expect(result.ready).toBe(true)
    expect(await readdir(root)).toEqual(['rainy_bytecode_fixture.py'])
  })
  it('rejects a Windows interpreter in a Linux execution world despite its mapped path', async () => {
    const path = join(await directory(), 'python.exe')
    const candidate = await probeRuntimeCandidate({ targetId: 'wsl:test', platform: 'linux', language: 'python', path,
      source: 'manual', timeoutMs: 1000, run: async () => JSON.stringify({ platform: 'windows', version: '3.12.14', executable: path, capabilities: [] }) })
    expect(candidate.ready).toBe(false)
    expect(candidate.error).toContain('workspace runs on linux')
  })
  it('keeps missing scientific libraries separate from interpreter readiness', async () => {
    const path = join(await directory(), 'python.exe')
    const candidate = await probeRuntimeCandidate({ targetId: 'windows-local', platform: 'windows', language: 'python', path,
      source: 'manual', timeoutMs: 1000, run: async () => JSON.stringify({ platform: 'windows', version: '3.12.14', executable: path,
        capabilities: [{ name: 'python', ready: true }, { name: 'torch-cuda', ready: false, detail: 'No compatible GPU' }] }) })
    expect(candidate.ready).toBe(true)
    expect(candidate.capabilities.find(item => item.name === 'torch-cuda')?.ready).toBe(false)
  })
  it('rejects a compiler producing Windows programs in a Linux execution world', async () => {
    const candidate = await probeRuntimeCandidate({ targetId: 'wsl:test', platform: 'linux', language: 'cpp', path: join(await directory(), 'clang.exe'),
      source: 'manual', timeoutMs: 1000, run: async command => command.arguments[0] === '--version' ? 'clang version 23.1.2' : 'x86_64-w64-windows-gnu' })
    expect(candidate.ready).toBe(false)
    expect(candidate.error).toContain('another execution platform')
  })
  it('persists per-workspace choices without changing the process environment', async () => {
    const root = await directory()
    const first = WorkspaceId('first')
    const second = WorkspaceId('second')
    const roots = new Map([[first, { path: join(root, 'first') }], [second, { path: join(root, 'second') }]])
    const before = process.env.PATH
    const options = { root, targetId: 'windows-local', platform: 'windows' as const, probeTimeoutMs: 1000, maxCandidates: 4,
      resolveWorkspace: (id: WorkspaceId) => roots.get(id), run: async (command: { executable: string }) => JSON.stringify({
        platform: 'windows', version: '3.12.14', executable: command.executable, prefix: join(root, 'env'), capabilities: [{ name: 'python', ready: true }],
      }) }
    const runtime = new RuntimeEnvironments(options)
    await runtime.initialize()
    await expect(runtime.status(WorkspaceId('missing'))).rejects.toThrow('The selected workspace does not exist')
    const executable = join(root, 'env', 'python.exe')
    await runtime.select(first, 'python', executable)
    expect(runtime.resolve(join(root, 'first')).executables.python).toBe(executable)
    expect(runtime.resolve(join(root, 'second')).executables.python).toBeUndefined()
    expect(process.env.PATH).toBe(before)
    const reopened = new RuntimeEnvironments(options)
    await reopened.initialize()
    expect(reopened.resolve(join(root, 'first')).executables.python).toBe(executable)
    await reopened.select(first, 'python', null)
    expect(reopened.resolve(join(root, 'first')).executables.python).toBeUndefined()
  })

  it('probes discovered interpreters concurrently within the configured bound and keeps candidate order', async () => {
    const root = await directory()
    const workspace = WorkspaceId('discovery')
    const project = join(root, 'project')
    for (const name of ['.venv', 'venv']) {
      await mkdir(join(project, name, 'Scripts'), { recursive: true })
      await writeFile(join(project, name, 'Scripts', 'python.exe'), '')
    }
    let inFlight = 0
    let peak = 0
    const started: string[] = []
    const runtime = new RuntimeEnvironments({ root, targetId: 'windows-local', platform: 'windows', probeTimeoutMs: 1000, maxCandidates: 16,
      probeConcurrency: 2, resolveWorkspace: id => id === workspace ? { path: project } : undefined,
      run: async (command) => {
        started.push(command.executable)
        peak = Math.max(peak, ++inFlight)
        await new Promise(resolve => setTimeout(resolve, 15))
        inFlight--
        return JSON.stringify({ platform: 'windows', version: '3.12.14', executable: command.executable, capabilities: [] })
      } })
    await runtime.initialize()
    const snapshot = await runtime.discover(workspace)
    expect(started.length).toBeGreaterThanOrEqual(2)
    expect(peak).toBe(2)
    expect(snapshot.candidates.map(candidate => candidate.path)).toEqual(started.filter((path, index) => started.indexOf(path) === index))
    expect(snapshot.candidates.slice(0, 2).map(candidate => candidate.source)).toEqual(['project', 'project'])
  })

  it('offers the carrier PHP by default until a workspace selects another interpreter', async () => {
    const root = await directory()
    const workspace = WorkspaceId('php')
    const project = join(root, 'project')
    const builtin = join(root, 'resources', 'php', 'php.exe')
    await mkdir(project, { recursive: true })
    await mkdir(join(root, 'resources', 'php'), { recursive: true })
    await writeFile(builtin, '')
    const runtime = new RuntimeEnvironments({ root, targetId: 'windows-local', platform: 'windows', probeTimeoutMs: 1000, maxCandidates: 16,
      builtinExecutables: { php: builtin }, resolveWorkspace: id => id === workspace ? { path: project } : undefined,
      run: async command => JSON.stringify({ platform: 'windows', version: '8.4.14', executable: command.executable, capabilities: [] }) })
    await runtime.initialize()
    const snapshot = await runtime.discover(workspace)
    expect(snapshot.candidates.find(candidate => candidate.path === builtin)).toMatchObject({ language: 'php', source: 'bundled', ready: true })
    const resolved = runtime.resolve(project)
    expect(resolved.executables.php).toBe(builtin)
    expect(resolved.environment.RAINY_PHP_EXTENSION_DIR).toBe(join(root, 'resources', 'php', 'ext'))
    const selected = join(root, 'php-8.5', 'php.exe')
    await runtime.select(workspace, 'php', selected)
    expect(runtime.resolve(project).executables.php).toBe(selected)
  })

  it('ignores the built-in PHP until its component is downloaded', async () => {
    const root = await directory()
    const project = join(root, 'project')
    const builtin = join(root, 'modules', 'php', 'php.exe')
    await mkdir(project, { recursive: true })
    const runtime = new RuntimeEnvironments({ root, targetId: 'windows-local', platform: 'windows', probeTimeoutMs: 1000, maxCandidates: 16,
      builtinExecutables: { php: builtin }, resolveWorkspace: () => ({ path: project }),
      run: async command => JSON.stringify({ platform: 'windows', version: '8.5.0', executable: command.executable, capabilities: [] }) })
    await runtime.initialize()
    expect(runtime.resolve(project).executables.php).toBeUndefined()
    await mkdir(dirname(builtin), { recursive: true })
    await writeFile(builtin, '')
    expect(runtime.resolve(project).executables.php).toBe(builtin)
  })
})

describe('carrier project continuity', () => {
  it('retains the original workspace identity and binds another target without rewriting the source', async () => {
    const root = await directory()
    const old = createProjectRegistry({ root, targetId: 'wsl:original' })
    const id = await old.getOrRegister({ workspaceId: WorkspaceId('original-workspace-id'), path: '/mnt/c/project', title: 'Project' })
    expect(id).toBe('original-workspace-id')
    const native = createProjectRegistry({ root, targetId: 'windows-local' })
    await native.getOrRegister({ workspaceId: WorkspaceId('native-workspace-id'), path: 'C:\\project', title: 'Project', projectId: id })
    expect(await old.projectForWorkspace(WorkspaceId('original-workspace-id'))).toBe(id)
    expect(await native.projectForWorkspace(WorkspaceId('native-workspace-id'))).toBe(id)
    expect((await native.list())[0]?.bindings).toHaveLength(2)
  })
  it('serializes simultaneous registrations through the shared in-process owner', async () => {
    const root = await directory()
    const registry = createProjectRegistry({ root, targetId: 'windows-local' })
    expect(createProjectRegistry({ root, targetId: 'windows-local' })).toBe(registry)
    await Promise.all(Array.from({ length: 8 }, (_, index) => registry.getOrRegister({ workspaceId: WorkspaceId(`project-${index}`), path: `C:\\project-${index}`, title: `Project ${index}` })))
    expect((await registry.list()).map(item => item.projectId)).toHaveLength(8)
    expect(JSON.parse(await readFile(join(root, 'projects.json'), 'utf8'))).toMatchObject({ version: 1 })
  })
  it('defaults only new installations to native Windows while preserving the old WSL choice and unrelated settings', async () => {
    expect(savedExecutionTarget({})).toEqual(WINDOWS_TARGET)
    expect(savedExecutionTarget({ distro: 'Ubuntu' })).toMatchObject({ kind: 'wsl', distro: 'Ubuntu' })
    const path = join(await directory(), 'desktop.json')
    await saveExecutionTarget(path, { id: ExecutionTargetId('wsl:test'), kind: 'wsl', label: 'Ubuntu', distro: 'Ubuntu' })
    await saveExecutionTarget(path, WINDOWS_TARGET)
    expect(await readDesktopPreferences(path)).toMatchObject({ distro: 'Ubuntu', executionTarget: WINDOWS_TARGET })
  })
})

describe('native Windows IDE commands', () => {
  it('uses native paths and preserves literal arguments without invoking WSL', async () => {
    const root = 'C:\\Project Space'
    const executable = 'C:\\Python\\python.exe'
    const result = await resolveIdeRun({
      subprocess: { terminalEnvironment: async () => ({ platform: 'windows' }), resolveExecutable: async value => value },
      resources: { node: 'C:\\Node\\node.exe', tsxImport: 'C:\\Node\\tsx.mjs', resourceRoot: 'C:\\Rainy\\ide' },
      files: { realpath: async path => path, kind: async path => path === root ? 'directory' : path === root + '\\main.py' ? 'file' : undefined,
        readText: async () => '', createBuildDirectory: async () => {}, removeBuildDirectory: async () => {} },
      resolveWorkspace: async workspaceId => ({ workspaceId, root }), maxConfigurationBytes: 4096,
      resolveEnvironment: () => ({ executables: { python: executable }, environment: { PATH: 'C:\\Python' } }),
    }, WorkspaceId('windows-project'), { name: 'Python', language: 'python', program: 'main.py', arguments: ['a b', '; literal'] })
    expect(result.spec.launch).toEqual({ argv: [executable, '-u', root + '\\main.py', 'a b', '; literal'], cwd: root, environment: { PATH: 'C:\\Python' } })
  })
})
