/** Host entry: build every module, announce readiness to the carrier, and serve until stopped. */
import { mkdirSync, rmSync, writeFileSync } from 'node:fs'
import { delimiter, dirname, join } from 'node:path'
import { z } from 'zod'
import { brandString } from '../shared/brand.ts'
import { resolveConfig } from '../shared/config.ts'
import type { IdeRootId, WorkspaceId } from '../shared/ide-files-protocol.ts'
import { createProjectRegistry, ExecutionTargetId, ProjectId } from '../shared/project-registry.ts'
import { Activity } from './activity.ts'
import { createAgent } from './agent/index.ts'
import { sendControl, serveControl } from './control.ts'
import { readHostEnvironment } from './env.ts'
import { installIceSky } from './icesky/index.ts'
import { createIde } from './ide/index.ts'
import { Projects, roots } from './projects.ts'
import { RpcHub } from './rpc.ts'
import { createRuntime } from './runtime/index.ts'
import { HostServer, readBody, sendJson } from './server.ts'
import { Settings } from './settings.ts'

// stdout belongs to the control channel; everything else is diagnostics.
console.log = console.error
console.info = console.error

const log = (message: string): void => { console.error(`[host] ${message}`) }

async function main(): Promise<void> {
  const env = readHostEnvironment(process.env, import.meta.url)
  mkdirSync(env.home, { recursive: true, mode: 0o700 })
  mkdirSync(env.tmp, { recursive: true, mode: 0o700 })
  process.env.PATH = [dirname(process.execPath), join(env.appRoot, 'bin'), process.env.PATH].filter(Boolean).join(delimiter)

  const settings = new Settings(env.home, process.env)
  await settings.load()
  const config = resolveConfig(settings.get().workbench)
  const projects = new Projects(join(env.home, 'projects.json'))
  await projects.load()
  const registry = createProjectRegistry({ root: env.carrierStateRoot, targetId: env.executionTargetId })
  const activity = new Activity()
  const rpc = new RpcHub()
  const server = new HostServer({
    port: env.port,
    rendererRoot: join(env.appRoot, 'dist', 'renderer'),
    injectedGlobals: () => ({
      __RAINY_AGENT__: { name: 'RainyAgent', version: env.version, environment: env.platform === 'win32' ? 'Windows' : 'WSL' },
      __RAINY_WORKBENCH_CONFIG__: config,
    }),
    log,
  })

  const runtime = await createRuntime({ env, projects, registry, activity, log })
  const ide = await createIde({ env, config, projects, runtime, server, activity, log })
  const agent = await createAgent({ env, settings, projects, registry, runtime, rpc, activity, log })
  await installIceSky({ env, server, log })

  rpc.register('app.info', () => ({
    name: 'RainyAgent' as const, version: env.version, environment: env.platform === 'win32' ? 'Windows' as const : 'WSL' as const,
    executionTargetId: env.executionTargetId, platform: env.platform, home: env.home,
  }))
  rpc.register('prefs.get', () => settings.prefs())
  rpc.register('prefs.set', async (change) => {
    const prefs = await settings.setPrefs(change)
    rpc.emit('prefs.changed', prefs)
    return prefs
  })
  rpc.register('ide', request => ide.handle(request))
  rpc.register('runtime', request => runtime.handle(request))

  server.socket('/rpc', (socket) => { rpc.attach(socket) })
  server.route('/rainy/icon.png', async (_request, response) => {
    const { readFile } = await import('node:fs/promises')
    response.writeHead(200, { 'Content-Type': 'image/png', 'Cache-Control': 'private, max-age=86400' })
    response.end(await readFile(join(env.resources, 'icon.png')))
  })
  // The carrier reads models and connects Strata through this endpoint with the window's cookie.
  server.route('/rainy/control', async (request, response) => {
    try {
      if (request.method === 'GET') { sendJson(response, 200, agent.controlStatus()); return }
      if (request.method !== 'POST') { response.writeHead(405); response.end(); return }
      const command: unknown = JSON.parse((await readBody(request, 65536)).toString('utf8'))
      if (command === null || typeof command !== 'object' || !('method' in command) || command.method !== 'connect-strata' || !('params' in command)) {
        throw new Error('未知设置操作。')
      }
      sendJson(response, 200, { result: await agent.connectStrata(command.params) })
    } catch (error) {
      sendJson(response, 400, { error: error instanceof Error ? error.message : '操作失败。' })
    }
  })

  await restorePendingProject()
  for (const project of projects.list()) await registry.getOrRegister({ workspaceId: project.id, path: project.path, title: project.title })
  projects.onChange((project) => { void registry.getOrRegister({ workspaceId: project.id, path: project.path, title: project.title }) })

  await server.listen()
  let stopping: Promise<void> | undefined
  const stop = (): Promise<void> => {
    stopping ??= (async () => {
      rpc.close()
      await Promise.allSettled([agent.close(), ide.close()])
      await server.close()
      rmSync(env.tmp, { recursive: true, force: true })
      process.exit(0)
    })()
    return stopping
  }
  serveControl({
    inspectActivity: mode => activity.inspect(mode),
    inspectProject: async (requested) => {
      const id = requested === undefined ? ide.selection() : brandString<WorkspaceId>(requested)
      if (id === null) return null
      const project = projects.get(id)
      if (project === undefined) throw new Error('workspace-unavailable')
      const projectId = await registry.getOrRegister({ workspaceId: project.id, path: project.path, title: project.title })
      return { projectId, workspaceId: project.id, roots: roots(project) }
    },
    stop,
  }, process.env.RAINY_DETACHED !== '1')
  process.on('SIGTERM', () => { void stop() })

  const ready = { type: 'ready', protocol: 1, url: server.launchUrl(), pid: process.pid, home: env.home }
  if (process.env.RAINY_READY_FILE) writeFileSync(process.env.RAINY_READY_FILE, JSON.stringify(ready), { mode: 0o600 })
  sendControl(ready)
  log(`ready on 127.0.0.1:${server.port}`)

  /** Apply the project the carrier moved here from another execution target. */
  async function restorePendingProject(): Promise<void> {
    const projectId = process.env.RAINY_PENDING_PROJECT_ID
    const path = process.env.RAINY_PENDING_PROJECT_PATH
    if (!projectId || !path) return
    const attached = process.env.RAINY_PENDING_PROJECT_ROOTS
      ? z.array(z.object({ rootId: z.string().min(1), path: z.string().min(1), title: z.string() }).strict()).parse(JSON.parse(process.env.RAINY_PENDING_PROJECT_ROOTS))
      : []
    const project = await projects.open(path)
    const id = await registry.getOrRegister({ workspaceId: project.id, path: project.path, title: project.title, projectId: ProjectId(projectId) })
    for (const root of attached) await projects.attach(project.id, root.path, { rootId: brandString<IdeRootId>(root.rootId), title: root.title })
    await registry.selectTarget(id, ExecutionTargetId(env.executionTargetId))
    await ide.setSelection(project.id)
  }
}

main().catch((error: unknown) => {
  log(error instanceof Error ? error.stack ?? error.message : String(error))
  sendControl({ type: 'fatal', message: error instanceof Error ? error.message : 'Host startup failed' })
  process.exitCode = 1
})
