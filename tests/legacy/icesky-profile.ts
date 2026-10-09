/** Private UI fixture: real Rainy profile, ten durable Sessions, and one loopback-only provider. */
import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { mkdir, mkdtemp, readFile, readdir, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import type { IncomingMessage, ServerResponse } from 'node:http'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import { createInterface } from 'node:readline'
import { initProfile, loadLayeredEnv, loadProfileDirectory } from '@deepseek-ai/dsh-app-boot'
import { createUserMessage } from '@deepseek-ai/dsh-llm'
import { SessionId } from '@deepseek-ai/dsh-session'
import type {} from '@deepseek-ai/dsh-agent'
import type {} from '@deepseek-ai/dsh-client-connection'
import type {} from '@deepseek-ai/dsh-host-webserver'
import type {} from '@deepseek-ai/dsh-session-persistence'
import type {} from '@deepseek-ai/dsh-session-projection-cache'
import type {} from '@deepseek-ai/dsh-session-title'
import type {} from '@deepseek-ai/dsh-workspace'
import { runProfile } from '../../cli/src/profile-boot.ts'
import { configureModel, configuredModels } from '../src/models.ts'

const fixtureKind = 'rainy-icesky-ui-fixture'
const provider = 'local'
const model = 'rainy-icesky-fixture'
const assistantCode = process.env.RAINY_IDE_CODE_FIXTURE === '1'
const fixtureReply = assistantCode ? '```python\nvalue = 41\nprint(value + 1)\n```' : 'Fixture ready'
const fixturePrompt = assistantCode ? `Reply with exactly this code block:\n\n${fixtureReply}` : 'Reply with Fixture ready.'
const fixtureSessionIds = Array.from({ length: 10 }, (_, index) => SessionId(`icesky-ui-fixture-${index + 1}`))
const requestedHome = process.env.RAINY_HOME?.trim()
const home = requestedHome ? resolve(requestedHome) : await mkdtemp(join(tmpdir(), 'rainy-icesky-ui-'))
await mkdir(home, { recursive: true, mode: 0o700 })
const markerPath = join(home, '.icesky-ui-fixture.json')
if (!existsSync(markerPath)) {
  assert.equal((await readdir(home)).length, 0, 'Fixture home must be empty or carry its fixture marker')
  await writeFile(markerPath, JSON.stringify({ kind: fixtureKind, version: 1, ...(assistantCode ? { assistantCode: true } : {}) }) + '\n', { flag: 'wx', mode: 0o600 })
}
const marker: unknown = JSON.parse(await readFile(markerPath, 'utf8'))
assert(marker !== null && typeof marker === 'object' && !Array.isArray(marker)
  && 'kind' in marker && marker.kind === fixtureKind && 'version' in marker && marker.version === 1,
'Fixture home marker is invalid')
const markedCode = 'assistantCode' in marker ? marker.assistantCode : false
assert.equal(typeof markedCode, 'boolean', 'Fixture home code mode must be a boolean')
assert.equal(markedCode, assistantCode, 'Fixture home mode differs; use a separate private home for code replies')

const workspace = join(home, 'workspace')
await mkdir(workspace, { recursive: true })
process.chdir(workspace)
process.env = Object.fromEntries(Object.entries(process.env).filter(([name]) =>
  !/(?:KEY|TOKEN|SECRET|PASSWORD|CREDENTIAL)/i.test(name)
  && !/^(?:HTTP_PROXY|HTTPS_PROXY|ALL_PROXY)$/i.test(name)))
process.env.DSH_HOME = home
process.env.RAINY_HOME = home
process.env.DSH_TELEMETRY_DISABLED = '1'
process.env.NO_PROXY = '127.0.0.1,localhost'

const input = createInterface({ input: process.stdin, crlfDelay: Infinity })
let requestStop: () => void = () => {}
const stopped = new Promise<void>((resolveStop) => { requestStop = resolveStop })
input.on('line', (line) => {
  try {
    const command: unknown = JSON.parse(line)
    if (command !== null && typeof command === 'object' && 'type' in command && command.type === 'stop') requestStop()
  } catch (error) {
    // Control input is private test data; malformed lines do not affect the profile.
    if (!(error instanceof SyntaxError)) throw error
  }
})
input.on('close', requestStop)

let modelRequests = 0
async function respondFixture(request: IncomingMessage, response: ServerResponse): Promise<void> {
  if (request.method === 'GET' && request.url === '/v1/models') {
    response.writeHead(200, { 'Content-Type': 'application/json' })
    response.end(JSON.stringify({ data: [{ id: model }] }))
    return
  }
  if (request.method !== 'POST' || request.url !== '/v1/chat/completions') {
    response.writeHead(404)
    response.end()
    return
  }
  try {
    for await (const _chunk of request) { /* No request content is retained or printed. */ }
    modelRequests++
    response.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' })
    const chunk = { id: `chatcmpl-fixture-${modelRequests}`, object: 'chat.completion.chunk', created: 1, model,
      choices: [{ index: 0, delta: { role: 'assistant', content: fixtureReply }, finish_reason: 'stop' }] }
    response.write(`data: ${JSON.stringify(chunk)}\n\n`)
    response.write(`data: ${JSON.stringify({ ...chunk, choices: [], usage: { prompt_tokens: 1, completion_tokens: 2, total_tokens: 3 } })}\n\n`)
    response.end('data: [DONE]\n\n')
  } catch (error) {
    if (!request.destroyed) response.destroy(error instanceof Error ? error : undefined)
  }
}
const fixtureServer = createServer((request, response) => {
  void respondFixture(request, response).catch((error: unknown) => {
    response.destroy(error instanceof Error ? error : undefined)
  })
})
await new Promise<void>((resolveListen, reject) => {
  fixtureServer.once('error', reject)
  fixtureServer.listen(0, '127.0.0.1', resolveListen)
})
const fixtureAddress = fixtureServer.address()
assert(fixtureAddress && typeof fixtureAddress === 'object')
const baseURL = `http://127.0.0.1:${fixtureAddress.port}/v1`
const app = resolve(import.meta.dirname, '..')
const profileDir = join(home, 'profiles', 'rainy')
let running: Awaited<ReturnType<typeof runProfile>> | undefined
const send = (value: object): void => { process.stdout.write(`RAINY_CONTROL ${JSON.stringify(value)}\n`) }
try {
  if (!existsSync(join(profileDir, 'package.json'))) initProfile(profileDir, ['@deepseek-ai/dsh-rainy-desktop'])
  const profile = loadProfileDirectory('dsh', profileDir, join(app, 'package.json'))
  assert.equal(profile.skippedBundles.length, 0, 'Rainy fixture bundle must resolve')
  running = await runProfile({ environment: loadLayeredEnv('dsh', workspace), profile: 'rainy', args: [], patchFiles: [],
    resolvedProfile: { profile, installAnchor: join(app, 'package.json') } })
  const { ctx } = running
  await ctx.loader.await()
  await configureModel(ctx, { provider, model, local: true, baseURL, api: 'openai-completions',
    contextWindow: 32768, maxTokens: 1024, thinking: 'off' })
  ctx.on('llm/stream', async function* (request, next) {
    assert.equal(request.provider, provider, 'Fixture may call only its loopback provider')
    assert.equal(request.model, model, 'Fixture may call only its deterministic model')
    assert(configuredModels(ctx).some(value => value.provider === provider && value.model === model
      && value.baseURL === baseURL && value.api === 'openai-completions'), 'Fixture provider must remain on loopback')
    for await (const chunk of next()) yield chunk
  }, { global: true, prepend: true })
  const fixtureWorkspace = await ctx.workspaceRegistry.initializeDefault(() => Promise.resolve(workspace))
    ?? ctx.workspaceRegistry.list().find(value => value.path === workspace)
  assert(fixtureWorkspace, 'Fixture workspace must retain its private registration')
  let seededSessions = 0
  let reusedSessions = 0
  for (const [index, id] of fixtureSessionIds.entries()) {
    const existing = await ctx.sessionPersistence.stat(id)
    if (existing !== undefined) {
      assert.equal(existing.header.id, id, 'Fixture Session header must match its fixed id')
      assert.equal(existing.header.cwd, workspace, 'Fixture Session must belong to its private workspace')
      await fixtureWorkspace.attachSession(id)
      reusedSessions++
      continue
    }
    const handle = await ctx.agents.create({ sessionId: id, meta: { cwd: workspace }, agentOptions: { provider, model } })
    try {
      handle.agent.followup(createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: fixturePrompt }] }))
      const timeout = setTimeout(() => { requestStop() }, 60000)
      try {
        await Promise.race([handle.agent.whenIdle(), stopped.then(() => { throw new Error('Fixture seeding interrupted') })])
      } finally { clearTimeout(timeout) }
      assert(handle.agent.session.deriveMessages().some(message => message.role === 'assistant'
        && message.content.some(block => block.type === 'text' && block.text === fixtureReply)),
      'Fixture must contain its harmless completion')
      ctx.sessionTitle.rename(handle.agent.session, `IceSky fixture ${index + 1}`)
      await ctx.sessionProjectionCache.write(handle.agent.session)
      await fixtureWorkspace.attachSession(id)
      seededSessions++
    } finally { await handle.dispose() }
  }
  send({ type: 'ready', protocol: 1, url: ctx.connection.authenticatedUrl(`http://127.0.0.1:${ctx.webServer.port}`),
    pid: process.pid, home, fixtureSessionIds, seededSessions, reusedSessions, modelRequests, assistantCode })
  await stopped
} catch (error) {
  send({ type: 'fatal', message: error instanceof Error ? error.message : 'IceSky fixture startup failed' })
  process.exitCode = 1
} finally {
  input.close()
  process.stdin.pause()
  await running?.shutdown.shutdown(process.exitCode === 1 ? 1 : 0)
  fixtureServer.closeAllConnections()
  await new Promise<void>((resolveClose) => { fixtureServer.close(() => { resolveClose() }) })
}
