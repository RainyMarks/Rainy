/** The assembled IDE behind the `ide` RPC method and the language WebSocket. */
import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { pathToFileURL } from 'node:url'
import { afterEach, describe, expect, it, vi } from 'vitest'
import WebSocket from 'ws'
import { DEFAULT_CONFIG } from '../../src/shared/config.ts'
import type { IdeExecutionStatus } from '../../src/shared/ide-execution-protocol.ts'
import type { IdeFileDocument, IdeWorkspace, IdeWorkspaceState } from '../../src/shared/ide-files-protocol.ts'
import { createProjectRegistry } from '../../src/shared/project-registry.ts'
import { Activity } from '../../src/host/activity.ts'
import type { HostEnvironment } from '../../src/host/env.ts'
import { createIde } from '../../src/host/ide/index.ts'
import { Projects } from '../../src/host/projects.ts'
import { RpcError } from '../../src/host/rpc.ts'
import { createRuntime } from '../../src/host/runtime/index.ts'
import { HostServer } from '../../src/host/server.ts'

const cleanups: Array<() => Promise<void>> = []
afterEach(async () => {
  for (const cleanup of cleanups.splice(0).reverse()) await cleanup()
})

async function host() {
  const root = await realpath(await mkdtemp(join(tmpdir(), 'rainy-ide-host-')))
  cleanups.push(() => rm(root, { recursive: true, force: true }))
  const env: HostEnvironment = {
    version: '0.0.0-test', home: join(root, 'home'), appRoot: process.cwd(), resources: join(root, 'resources'),
    platform: process.platform === 'win32' ? 'win32' : 'linux', executionTargetId: 'wsl:test', carrierStateRoot: join(root, 'carrier'),
    toolchainRoot: join(root, 'components'), tmp: join(root, 'tmp'), port: 0,
  }
  const project = join(root, 'project')
  await mkdir(project)
  const log = vi.fn()
  const projects = new Projects(join(env.home, 'projects.json'))
  await projects.load()
  const activity = new Activity()
  const registry = createProjectRegistry({ root: env.carrierStateRoot, targetId: env.executionTargetId })
  const runtime = await createRuntime({ env, projects, registry, activity, log })
  const server = new HostServer({ port: 0, rendererRoot: join(root, 'renderer'), injectedGlobals: () => ({}), log })
  const ide = await createIde({ env, config: DEFAULT_CONFIG, projects, runtime, server, activity, log })
  await server.listen()
  cleanups.push(async () => { await ide.close(); await server.close() })
  const launch = await fetch(server.launchUrl(), { redirect: 'manual' })
  const cookie = (launch.headers.get('set-cookie') ?? '').split(';')[0] ?? ''
  const origin = `http://127.0.0.1:${server.port}`
  const workspace = await ide.handle({ op: 'workspaces.open', path: project }) as IdeWorkspace
  return { root, env, project, projects, activity, server, ide, cookie, origin, workspace, log }
}

async function rpcError(promise: Promise<unknown>): Promise<RpcError> {
  try { await promise } catch (error) {
    if (error instanceof RpcError) return error
    throw error
  }
  throw new Error('The request succeeded.')
}

describe('ide RPC method', () => {
  it('reports IDE failures as RPC errors carrying the conflict observation', async () => {
    const { ide, project, workspace } = await host()
    const workspaceId = workspace.workspaceId
    await writeFile(join(project, 'main.py'), 'print(1)\n')
    const opened = await ide.handle({ op: 'files.read', workspaceId, path: 'main.py' }) as IdeFileDocument
    await writeFile(join(project, 'main.py'), 'print(2)\n')
    const conflict = await rpcError(ide.handle({ op: 'files.save', workspaceId, path: 'main.py', content: 'x', expectedVersion: opened.version }))
    expect(conflict).toMatchObject({ code: 'version-conflict', data: { currentVersion: expect.any(String) } })
    expect(conflict.data?.currentVersion).not.toBe(opened.version)
    const saved = await ide.handle({ op: 'state.save', workspaceId, baseRevision: 0, data: (await ide.handle({ op: 'state.read', workspaceId }) as IdeWorkspaceState).data }) as IdeWorkspaceState
    const stale = await rpcError(ide.handle({ op: 'state.save', workspaceId, baseRevision: 0, data: saved.data }))
    expect(stale).toMatchObject({ code: 'revision-conflict', data: { currentState: { revision: 1 } } })
    expect(await rpcError(ide.handle({ nothing: true }))).toMatchObject({ code: 'invalid-request' })
    expect(await rpcError(ide.handle({ op: 'files.read', workspaceId }))).toMatchObject({ code: 'invalid-request' })
    expect(await rpcError(ide.handle({ op: 'run.unknown', workspaceId }))).toMatchObject({ code: 'invalid-request' })
    expect(await rpcError(ide.handle({ op: 'files.read', workspaceId, path: 'x'.repeat(41 * 1024 * 1024) }))).toMatchObject({ code: 'too-large' })
  })

  it('remembers the selected project and forgets it when the project is removed', async () => {
    const { ide, workspace, project } = await host()
    expect(ide.selection()).toBeNull()
    await ide.setSelection(workspace.workspaceId)
    expect(ide.selection()).toBe(workspace.workspaceId)
    expect(await ide.handle({ op: 'state.selection.read' })).toEqual({ version: 1, workspaceId: workspace.workspaceId })
    const renamed = await ide.handle({ op: 'workspaces.rename', workspaceId: workspace.workspaceId, title: 'Renamed' }) as IdeWorkspace
    expect(renamed.title).toBe('Renamed')
    await expect(ide.handle({ op: 'workspaces.remove', workspaceId: workspace.workspaceId })).resolves.toEqual({ workspaceId: workspace.workspaceId, removed: true })
    expect(ide.selection()).toBeNull()
    expect(await ide.handle({ op: 'workspaces.list' })).toEqual([])
    const reopened = await ide.handle({ op: 'workspaces.open', path: project }) as IdeWorkspace
    expect(reopened.workspaceId).not.toBe(workspace.workspaceId)
  })

  it('gates new terminals and formatting while the carrier freezes the Host', async () => {
    const { ide, activity, workspace } = await host()
    expect(activity.inspect('freeze')).toBe(false)
    expect(await rpcError(ide.handle({ op: 'terminal.start', workspaceId: workspace.workspaceId, cols: 80, rows: 24 })))
      .toMatchObject({ code: 'execution-error', message: expect.stringContaining('执行环境正在切换') })
    expect(await rpcError(ide.handle({ op: 'format', workspaceId: workspace.workspaceId, path: 'a.js', text: 'a', language: 'javascript' })))
      .toMatchObject({ message: expect.stringContaining('执行环境正在切换') })
    activity.inspect('resume')
  })

  it.skipIf(process.platform === 'win32')('reports a running terminal as activity and stops it when its project is removed', async () => {
    const { ide, activity, workspace } = await host()
    const workspaceId = workspace.workspaceId
    await ide.handle({ op: 'terminal.start', workspaceId, cols: 80, rows: 24 })
    await vi.waitFor(async () => { expect((await ide.handle({ op: 'execution.status', workspaceId }) as IdeExecutionStatus).terminals[0]?.phase).toBe('running') }, { timeout: 15000 })
    expect(activity.inspect('freeze')).toBe(true)
    expect(activity.inspect('observe')).toBe(true)
    await ide.handle({ op: 'workspaces.remove', workspaceId })
    expect(activity.active()).toBe(false)
  })

  it('formats JavaScript with the bundled Prettier without writing the file', async () => {
    const { ide, workspace, project } = await host()
    await writeFile(join(project, 'a.js'), 'const  a={b:1}')
    const result = await ide.handle({ op: 'format', workspaceId: workspace.workspaceId, path: 'a.js', text: 'const  a={b:1}\r\n', language: 'javascript' })
    expect(result).toEqual({ text: 'const a = { b: 1 };\r\n' })
    expect(await rpcError(ide.handle({ op: 'format', workspaceId: workspace.workspaceId, path: '../a.js', text: '', language: 'javascript' })))
      .toMatchObject({ code: 'invalid-path' })
  }, 30000)
})

describe('language route', () => {
  it('runs a TypeScript language server for messages sent before the server started', async () => {
    const { origin, cookie, workspace, project } = await host()
    await writeFile(join(project, 'main.ts'), 'const answer: number = 42\n')
    const url = `${origin.replace('http', 'ws')}/rainy/ide/lsp?workspaceId=${encodeURIComponent(workspace.workspaceId)}&language=typescript`
    const socket = new WebSocket(url, { headers: { cookie, origin } })
    cleanups.push(async () => { socket.terminate() })
    const messages: unknown[] = []
    socket.on('message', (data) => { messages.push(JSON.parse(String(data))) })
    await new Promise<void>((resolve, reject) => { socket.once('open', () => { resolve() }); socket.once('error', reject) })
    socket.send(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { processId: 1, rootUri: pathToFileURL(project).href, capabilities: {} } }))
    await vi.waitFor(() => { expect(messages.some(message => (message as { id?: unknown }).id === 1)).toBe(true) }, { timeout: 30000, interval: 100 })
    expect(messages.find(message => (message as { id?: unknown }).id === 1)).toMatchObject({ result: { capabilities: expect.any(Object) } })
    const closed = new Promise<number>((resolve) => { socket.once('close', (code) => { resolve(code) }) })
    socket.send(JSON.stringify({ jsonrpc: '2.0', method: 'workspace/didChangeWorkspaceFolders', params: {} }))
    expect(await closed).toBe(1011)
  }, 60000)

  it('closes language connections with an invalid query or an unauthenticated upgrade', async () => {
    const { origin, cookie } = await host()
    const invalid = new WebSocket(`${origin.replace('http', 'ws')}/rainy/ide/lsp?language=cobol`, { headers: { cookie, origin } })
    expect(await new Promise<number>((resolve) => { invalid.once('close', (code) => { resolve(code) }) })).toBe(1008)
    const anonymous = new WebSocket(`${origin.replace('http', 'ws')}/rainy/ide/lsp?language=python`, { headers: { origin } })
    await expect(new Promise((resolve, reject) => { anonymous.once('open', resolve); anonymous.once('error', reject) })).rejects.toThrow('403')
  })
})
