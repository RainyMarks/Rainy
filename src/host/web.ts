/** Serve the original DSH Web client with Rainy's local status and setup endpoint. */
import { createRequire } from 'node:module'
import { dirname, join } from 'node:path'
import { readFile } from 'node:fs/promises'
import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import * as FrontendStatic from '@deepseek-ai/dsh-host-frontend-static'
import type {} from '@deepseek-ai/dsh-host-webserver'
import type {} from '@deepseek-ai/dsh-client-connection'
import type {} from './policy.ts'
import type {} from './extensions.ts'
import type {} from './project-memory.ts'
import type {} from '@deepseek-ai/dsh-session-title'
import { CLAUDE_HAIKU, CLAUDE_OPUS, configureModel, configuredModels, connectStrataModel, discoverModels, probeModel, DEEPSEEK_FLASH } from './models.ts'
import { SessionId } from '@deepseek-ai/dsh-session'
import { fileDiff } from './file-diff.ts'
import { installIceSkyProxy } from './icesky-proxy.ts'
import * as IceSkyState from './icesky-state.ts'
import { installIceSkyStatic } from './icesky-static.ts'
import { readRequestBytes } from './request-body.ts'

export const name = 'rainy-web'
export const inject = ['webServer', 'connection', 'rainy', 'configEditor', 'agentDefaultModel', 'llm', 'credentials', 'rainyExtensions', 'rainyMemory', 'agents', 'sessionTitle', 'storageDomain', 'sessionPersistence']

/** Local workbench storage and resource delivery budgets. */
export interface Config {
  /** Maximum complete multi-tool draft JSON request bytes. */
  iceSkyMaxDraftBytes: number
  /** Private cache lifetime for resources addressed by their complete content version. */
  iceSkyAssetMaxAgeSeconds: number
}
/** Validated local workbench budgets. */
export const Config: z<Config> = z.object({
  iceSkyMaxDraftBytes: z.number().min(1024).max(64 * 1024 * 1024).default(8 * 1024 * 1024),
  iceSkyAssetMaxAgeSeconds: z.number().min(0).max(31536000).default(31536000),
})

/** Mount authenticated product routes. Untrusted web origins cannot access model settings or status. */
export async function apply(ctx: Context, config: Config): Promise<void> {
  const packageMetadata: unknown = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'))
  if (packageMetadata === null || typeof packageMetadata !== 'object' || !('version' in packageMetadata) || typeof packageMetadata.version !== 'string') throw new Error('RainyAgent package version is unavailable')
  const version = packageMetadata.version
  installIceSkyProxy(ctx)
  ctx.plugin(IceSkyState, { maxDraftBytes: config.iceSkyMaxDraftBytes })
  await installIceSkyStatic(ctx, config.iceSkyAssetMaxAgeSeconds)
  const require = createRequire(import.meta.url)
  const distIndex = join(dirname(require.resolve('@deepseek-ai/dsh-web-frontend/package.json')), 'dist/index.html')
  ctx.plugin(FrontendStatic, { distIndex })
  ctx.on('webserver/index-inject', (table) => {
    table.push({ kind: 'global', name: '__RAINY_AGENT__', value: { name: 'RainyAgent', version, environment: process.platform === 'win32' ? 'Windows' : 'WSL' } })
  })
  ctx.effect(() => ctx.webServer.register({ kind: 'exact', path: '/rainy/control', handler: async (req, res) => {
    const admission = ctx.connection.admit(req)
    if ('rejection' in admission) { res.writeHead(admission.rejection); res.end(); return }
    res.setHeader('Content-Type', 'application/json; charset=utf-8')
    res.setHeader('Cache-Control', 'no-store')
    try {
      if (req.method === 'GET') {
        res.end(JSON.stringify({
          budgets: [...ctx.rainy.budgets.values()], models: configuredModels(ctx), selected: ctx.agentDefaultModel.currentSelection(),
          sessions: ctx.agents.list().map(agent => ({
            id: agent.id, title: ctx.sessionTitle.get(agent.session)?.title, status: agent.status,
          })),
          presets: [{ name: 'DeepSeek', model: DEEPSEEK_FLASH }, { name: 'Claude Opus', model: CLAUDE_OPUS }, { name: 'Claude Haiku', model: CLAUDE_HAIKU }], tools: ['read', 'write', 'edit', process.platform === 'win32' ? 'pwsh' : 'bash'],
          globalPrompt: ctx.rainy.globalPrompt(),
        }))
        return
      }
      if (req.method !== 'POST') { res.writeHead(405); res.end(); return }
      const body = await readRequestBytes(req, 65536, () => new Error('设置请求过大。'))
      const command: unknown = JSON.parse(body.toString('utf8'))
      if (command === null || typeof command !== 'object' || !('method' in command) || !('params' in command)) throw new Error('设置请求无效。')
      let result: unknown
      switch (command.method) {
        case 'file-diff': {
          const params = command.params
          if (params === null || typeof params !== 'object' || !('sessionId' in params) || typeof params.sessionId !== 'string' || !('path' in params) || typeof params.path !== 'string') throw new Error('文件请求无效。')
          const cwd = ctx.agents.get(SessionId(params.sessionId))?.session.header.cwd
          if (!cwd) throw new Error('当前会话没有项目目录。')
          result = { text: await fileDiff(cwd, params.path) }; break
        }
        case 'configure-model': result = await configureModel(ctx, command.params); break
        case 'connect-strata': result = await connectStrataModel(ctx, command.params); break
        case 'discover-models': result = await discoverModels(ctx, command.params); break
        case 'probe-model': result = await probeModel(ctx, command.params); break
        case 'preview-budget': result = await ctx.rainy.previewBudget(command.params); break
        case 'configure-global-prompt': result = await ctx.rainy.saveGlobalPrompt(command.params); break
        case 'project-memory-status': result = await ctx.rainyMemory.status(command.params); break
        case 'project-memory-set-enabled': result = await ctx.rainyMemory.setEnabled(command.params); break
        case 'project-memory-edit': result = await ctx.rainyMemory.edit(command.params); break
        case 'project-memory-delete': result = await ctx.rainyMemory.remove(command.params); break
        case 'project-memory-clear': result = await ctx.rainyMemory.clear(command.params); break
        case 'extensions-catalog': {
          if (typeof command.params !== 'string') throw new Error('请选择会话。')
          result = await ctx.rainyExtensions.catalog(command.params); break
        }
        case 'extensions-select': {
          const params = command.params
          if (params === null || typeof params !== 'object' || !('sessionId' in params) || typeof params.sessionId !== 'string' || !('selection' in params)) throw new Error('扩展设置无效。')
          result = await ctx.rainyExtensions.select(params.sessionId, params.selection); break
        }
        case 'extensions-ida': {
          const params = command.params
          if (params === null || typeof params !== 'object' || !('sessionId' in params) || typeof params.sessionId !== 'string'
            || !('enabled' in params) || typeof params.enabled !== 'boolean') throw new Error('请选择会话和 IDA 启用状态。')
          result = await ctx.rainyExtensions.setIda(params.sessionId, params.enabled); break
        }
        default: throw new Error('未知设置操作。')
      }
      res.end(JSON.stringify({ result }))
    } catch (error) { res.statusCode = 400; res.end(JSON.stringify({ error: error instanceof Error ? error.message : '操作失败。' })) }
  } }))
  ctx.effect(() => ctx.webServer.register({ kind: 'exact', path: '/rainy/icon.png', handler: async (req, res) => {
    if ('rejection' in ctx.connection.admit(req)) { res.writeHead(403); res.end(); return }
    res.setHeader('Content-Type', 'image/png')
    res.end(await readFile(new URL('../resources/icon.png', import.meta.url)))
  } }))
}
