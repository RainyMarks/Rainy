/** Real Rainy profile with a deterministic external OpenAI-compatible server; all filesystem tools are real. */
import { createServer, type IncomingMessage, type ServerResponse } from 'node:http'
import { mkdtemp, mkdir, writeFile, readFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join, resolve } from 'node:path'
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import { initProfile, loadProfileDirectory, loadLayeredEnv } from '@deepseek-ai/dsh-app-boot'
import { runProfile } from '../../cli/src/profile-boot.ts'
import { createUserMessage, ToolCallId } from '@deepseek-ai/dsh-llm'
import { SessionId } from '@deepseek-ai/dsh-session'
import { assembleContextFor } from '@deepseek-ai/dsh-agent'
import type {} from '@deepseek-ai/dsh-system-prompt'
import type {} from '../src/policy.ts'
import type {} from '../src/extensions.ts'
import { configureModel } from '../src/models.ts'
import { estimateRequest, resolveBudget } from '../src/budget.ts'
import { fileDiff } from '../src/file-diff.ts'
import { verifyReadWindow } from './read-window.ts'

const checkpointFixture = JSON.parse(await readFile(new URL('../tests/fixtures/compaction-checkpoint.json', import.meta.url), 'utf8')) as {
  directive: string
  limits: Record<string, number>
  toolNames: string[]
  summary: string
  checkpointPrefix: string
  checkpointSuffix: string
}
const temporary = await mkdtemp(join(tmpdir(), 'rainy-composition-'))
const project = join(temporary, '中文 project')
const home = join(temporary, 'home')
const contextWindow = Number(process.env.RAINY_TEST_CONTEXT ?? 32768)
const outputTokens = resolveBudget(contextWindow).outputTokens
const shellTool = process.platform === 'win32' ? 'pwsh' : 'bash'
const coreToolNames = ['read', 'write', 'edit', shellTool].sort()
const originalTask = 'Read the requested fixture files. Preserve RAINY-CONSTRAINT-714. This is a deterministic test.'
await mkdir(project, { recursive: true })
await writeFile(join(project, 'sample.txt'), Array.from({ length: 60 }, (_, i) => `fixture-line-${i} ${'abcd '.repeat(30)}`).join('\n'))
await writeFile(join(project, 'recovery.txt'), Array.from({ length: 256 }, (_, i) => `recovery-line-${i} ${'abcd '.repeat(30)}`).join('\n'))
process.env.DSH_HOME = home
process.env.RAINY_HOME = home
process.env.RAINY_CARRIER_STATE_ROOT = join(temporary, 'carrier')
process.env.RAINY_EXECUTION_TARGET_ID = process.platform === 'win32' ? 'windows-local' : 'linux-local'
process.env.DSH_TELEMETRY_DISABLED = '1'
let calls = 0
let summaries = 0
let requests = 0
let mode: 'normal' | 'recovery-history' | 'overflow' | 'summary-failure' | 'disconnect' = 'normal'
let recoveryReads = 0
let rejectedRequests = 0
const schemas: string[][] = []
let serverFailure: Error | undefined
async function serve(request: IncomingMessage, response: ServerResponse): Promise<void> {
  if (request.url === '/v1/models') {
    assert.equal(request.headers.authorization, 'Bearer local-fixture-key')
    response.writeHead(200, { 'Content-Type': 'application/json' })
    response.end(JSON.stringify({ data: [{ id: 'rainy-fixture' }] }))
    return
  }
  let body = ''
  for await (const chunk of request) body += String(chunk)
  const value = JSON.parse(body) as {
    messages: Array<{ content?: string; role: string }>
    tools?: Array<{ function: { name: string } }>
    max_tokens: number
  }
  requests++
  const summary = String(value.messages.at(-1)?.content).includes('compaction engine')
  const names = (value.tools ?? []).map(tool => tool.function.name).sort()
  schemas.push(names)
  assert.deepEqual(names, coreToolNames)
  assert.equal(value.max_tokens, summary ? resolveBudget(contextWindow).summaryTokens : outputTokens)
  if (summary) {
    const limit = checkpointFixture.limits[String(contextWindow)]
    assert.equal(value.max_tokens, limit)
    assert.equal(value.messages.at(-1)?.content, checkpointFixture.directive
      .replace('<target>', String(Math.floor(limit / 2))).replace('<limit>', String(limit)))
    assert.deepEqual(names.map(name => name === shellTool ? 'shell' : name).sort(), checkpointFixture.toolNames)
    if (mode === 'normal') assert.equal(value.messages.filter(message => message.role === 'user' && message.content === originalTask).length, 1)
  }
  if ((mode === 'overflow' && !summary) || (mode === 'summary-failure' && summary)) {
    rejectedRequests++
    response.writeHead(400, { 'Content-Type': 'application/json' })
    response.end(JSON.stringify({ error: { code: mode === 'overflow' ? 'context_length_exceeded' : 'fixture_failure', message: mode === 'overflow' ? 'maximum context length exceeded' : 'Fixture summary failed; preserve history.' } }))
    return
  }
  response.writeHead(200, { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' })
  const send = (delta: object, finish: string | null = null) => response.write('data: ' + JSON.stringify({ id: `chatcmpl-${requests}`, object: 'chat.completion.chunk', created: 1, model: 'rainy-fixture', choices: [{ index: 0, delta, finish_reason: finish }] }) + '\n\n')
  if (mode === 'disconnect') {
    rejectedRequests++; send({ role: 'assistant', content: 'Interrupted fixture stream' })
    setTimeout(() => response.destroy(), 20); return
  }
  if (summary) {
    summaries++
    send({ role: 'assistant', content: checkpointFixture.summary }, 'stop')
  } else if (mode === 'recovery-history') {
    if (recoveryReads === 0) {
      recoveryReads++
      send({ role: 'assistant', tool_calls: [{ index: 0, id: 'recovery-read', type: 'function', function: { name: 'read', arguments: JSON.stringify({
        file_path: join(project, 'recovery.txt'), limit: Math.floor(contextWindow / 512),
      }) } }] }, 'tool_calls')
    } else send({ role: 'assistant', content: 'Verified the requested fixture page.' }, 'stop')
  } else if (calls < 50) {
    calls++
    send({ role: 'assistant', tool_calls: [{ index: 0, id: `call-${calls}`, type: 'function', function: { name: 'read', arguments: JSON.stringify({
      file_path: join(project, 'sample.txt'), limit: Math.max(1, Math.floor(contextWindow / 2048)),
    }) } }] }, 'tool_calls')
  } else send({ role: 'assistant', content: 'Completed fifty real file reads. RAINY-CONSTRAINT-714 retained.' }, 'stop')
  response.write('data: ' + JSON.stringify({ id: 'usage', object: 'chat.completion.chunk', created: 1, model: 'rainy-fixture', choices: [], usage: { prompt_tokens: Math.ceil(JSON.stringify(value.messages).length / 4), completion_tokens: 50, total_tokens: Math.ceil(JSON.stringify(value.messages).length / 4) + 50 } }) + '\n\n')
  response.end('data: [DONE]\n\n')
}
const server = createServer((request, response) => {
  void serve(request, response).catch((error: unknown) => {
    serverFailure = error instanceof Error ? error : new Error(String(error))
    response.destroy(serverFailure)
  })
})
await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
const address = server.address()
assert(address && typeof address === 'object')
const app = process.env.RAINY_TEST_APP ? resolve(process.env.RAINY_TEST_APP) : resolve(import.meta.dirname, '..')
const profileDir = join(home, 'profiles/rainy')
initProfile(profileDir, ['@deepseek-ai/dsh-rainy-desktop'])
const profile = loadProfileDirectory('dsh', profileDir, join(app, 'package.json'))
assert.equal(profile.skippedBundles.length, 0)
const { ctx, shutdown } = await runProfile({ environment: loadLayeredEnv('dsh'), profile: 'rainy', args: [], patchFiles: [], resolvedProfile: { profile, installAnchor: join(app, 'package.json') } })
const report: Record<string, unknown> = { kind: 'simulated-provider-real-dsh-composition', contextWindow }
try {
  const spillRoot = join(temporary, 'spill')
  const spillEntry = ctx.configEditor.entries().find(entry => entry.options.id === 'spill-local')
  assert(spillEntry)
  await ctx.configEditor.edit(spillEntry, () => ({ root: spillRoot, cleanupPeriodDays: 0 }))
  await configureModel(ctx, { provider: 'read-window-boundary', model: 'four-k', local: true,
    baseURL: `http://127.0.0.1:${address.port}/v1`, contextWindow: 4096, thinking: 'off' })
  await configureModel(ctx, { provider: 'local', model: 'rainy-fixture', local: true, baseURL: `http://127.0.0.1:${address.port}/v1`, contextWindow, maxTokens: outputTokens, thinking: 'off' })
  const observed: import('@deepseek-ai/dsh-session').SessionEvent[] = []
  const stopObserving = ctx.on('session/event', (session, event) => { if (session.id === 'rainy-composition') observed.push(event) })
  const handle = await ctx.agents.create({ sessionId: SessionId('rainy-composition'), meta: { cwd: project }, agentOptions: { provider: 'local', model: 'rainy-fixture' } })
  const agent = handle.agent
  agent.followup(createUserMessage({ content: [{ type: 'text', text: originalTask }], source: { kind: 'user' } }))
  await Promise.race([agent.whenIdle(), new Promise<never>((_, reject) => { const timeout = setTimeout(() =>{  reject(new Error('Composition run exceeded two minutes')) }, 120000); timeout.unref() })])
  const events = observed
  if (serverFailure) throw serverFailure
  const failedTools = events.filter(event => event.type === 'tool/result' && event.data.error !== undefined)
  assert.equal(failedTools.length, 0)
  assert.equal(calls, 50)
  assert(summaries >= 2, `Expected two compactions, received ${summaries}`)
  const history = agent.session.deriveMessages()
  assert(JSON.stringify(history).includes('RAINY-CONSTRAINT-714'))
  const originalUser = events.find(event => event.type === 'user/message' && event.data.source.kind === 'user')
  assert(originalUser?.type === 'user/message')
  assert.deepEqual(history.filter(message => message.id === originalUser.data.id), [originalUser.data])
  const checkpoints = events.filter(event => event.type === 'compaction/summary')
  for (const checkpoint of checkpoints) {
    assert.deepEqual(checkpoint.data.summary, [{ type: 'text', text: checkpointFixture.summary }])
    assert(!checkpoint.data.shadowedSeqs.includes(originalUser.seq), 'Auxiliary reference must never enter the replacement span')
  }
  for (const event of events) {
    if (event.type !== 'user/message' || event.data.source.kind !== 'compact-checkpoint') continue
    assert.deepEqual(event.data.content, [
      { type: 'text', text: checkpointFixture.checkpointPrefix },
      { type: 'text', text: checkpointFixture.summary },
      { type: 'text', text: checkpointFixture.checkpointSuffix },
    ])
  }
  report.checkpointReplay = { requests: summaries, checkpoints: checkpoints.length,
    exactOriginalInputRetained: true, exactAuxiliaryReference: true, referenceOutsideReplacement: true }
  const firstHeader = events.find(event => event.type === 'request/header')
  assert(firstHeader?.type === 'request/header')
  const coreTools = firstHeader.data.header.tools?.map(tool => tool.name).sort()
  assert.deepEqual(coreTools, coreToolNames)
  const initialSystem = events.find(event => event.type === 'system/message')
  const fixedTokens = estimateRequest({ messages: initialSystem?.type === 'system/message' ? [initialSystem.data.message] : [], tools: firstHeader.data.header.tools })
  assert(fixedTokens <= 4000, `Fixed prompt and schemas exceed budget: ${fixedTokens}`)
  report.calls = calls; report.summaries = summaries; report.requests = requests; report.fixedTokensEstimated = fixedTokens
  report.toolSchemas = coreTools; report.failedTools = failedTools.length
  report.readWindow = await verifyReadWindow(ctx, agent, project, spillRoot, () => requests)
  const id = agent.id
  await handle.dispose()
  const resumed = await ctx.agents.resume({ resumeSessionId: id, agentOptions: ctx.agentDefaultModel.currentSelection() })
  assert(JSON.stringify(resumed.agent.session.deriveMessages()).includes('RAINY-CONSTRAINT-714'))
  assert.deepEqual(resumed.agent.session.deriveMessages().filter(message => message.id === originalUser.data.id), [originalUser.data])
  assert.equal(resumed.agent.status, 'idle')
  report.resume = 'passed'
  const before = await resumed.agent.ctx.systemPrompt.assemble(assembleContextFor(resumed.agent))
  assert.deepEqual(before.tools.map(tool => tool.name).sort(), coreToolNames)
  const origin = `http://127.0.0.1:${ctx.webServer.port}`
  const login = await fetch(ctx.connection.authenticatedUrl(origin), { redirect: 'manual' })
  assert.equal(login.status, 303)
  const cookie = login.headers.get('set-cookie')?.split(';', 1)[0]
  assert(cookie)
  const iceSky = await fetch(`${origin}/rainy/icesky/index.html?embed=rainy`, { headers: { cookie } })
  assert.equal(iceSky.status, 200)
  const iceHtml = await iceSky.text()
  assert(iceHtml.includes('冰霄 IceSky'))
  assert(iceHtml.includes('css/rainy-embed.css'))
  const iceCss = await fetch(`${origin}/rainy/icesky/css/rainy-embed.css`, { headers: { cookie } })
  assert.equal(iceCss.status, 200)
  assert((await iceCss.text()).includes('.container > header'))
  const iceModels = await fetch(`${origin}/api/openai/models`, {
    headers: { cookie, 'x-openai-base-url': `http://127.0.0.1:${address.port}/v1`, 'x-openai-api-key': 'local-fixture-key' },
  })
  assert.equal(iceModels.status, 200)
  assert((await iceModels.text()).includes('rainy-fixture'))
  report.iceSkyStatic = 'passed'
  report.iceSkyRelay = 'passed'
  await mkdir(join(project, '.rainy/skills/example'), { recursive: true })
  const skillPath = join(project, '.rainy/skills/example/SKILL.md')
  await writeFile(skillPath, '---\ndescription: Verify SKILL-DESCRIPTION-19 after explicit selection.\n---\n\nRead the full skill when needed. Preserve SKILL-CHECK-19.')
  await ctx.rainyExtensions.select(id, { skills: ['project/example'], servers: [] })
  const enabled = await resumed.agent.ctx.systemPrompt.assemble(assembleContextFor(resumed.agent))
  assert(JSON.stringify(enabled).includes('SKILL-DESCRIPTION-19'))
  assert(enabled.sections.some(section => section.name === 'rainy-skill:project/example' && section.text.includes(JSON.stringify(skillPath))))
  assert(!JSON.stringify(enabled).includes('SKILL-CHECK-19'), 'Selecting a skill must not inject its full body')
  const skillBody = await ctx.tools.execute({ agent: resumed.agent, name: 'read', arguments: { file_path: skillPath }, callId: ToolCallId('skill-lazy-read'), signal: new AbortController().signal })
  assert.equal(skillBody.isError, false)
  assert(JSON.stringify(skillBody.content).includes('SKILL-CHECK-19'))
  await ctx.rainyExtensions.select(id, { skills: [], servers: [] })
  const disabled = await resumed.agent.ctx.systemPrompt.assemble(assembleContextFor(resumed.agent))
  assert(!JSON.stringify(disabled).includes('SKILL-DESCRIPTION-19'))
  report.skillIsolation = 'passed'
  report.skillLazyRead = 'passed'
  await ctx.rainyExtensions.select(id, { skills: [], servers: [{ serverName: 'fixture', transport: 'stdio', command: process.execPath, args: [join(app, 'lib/mcp-fixture.mjs')], tools: ['echo'] }] })
  const mcpAssembly = await resumed.agent.ctx.systemPrompt.assemble(assembleContextFor(resumed.agent))
  assert(mcpAssembly.tools.some(tool => tool.name === 'mcp__fixture__echo'))
  assert(!mcpAssembly.tools.some(tool => tool.name === 'mcp__fixture__unselected'))
  const other = await ctx.agents.create({ sessionId: SessionId('rainy-other-session'), meta: { cwd: project }, agentOptions: { provider: 'local', model: 'rainy-fixture' } })
  const otherAssembly = await other.agent.ctx.systemPrompt.assemble(assembleContextFor(other.agent))
  assert.deepEqual(otherAssembly.tools.map(tool => tool.name).sort(), coreToolNames)
  const echo = await ctx.tools.execute({ agent: resumed.agent, name: 'mcp__fixture__echo', arguments: { text: 'MCP-CHECK-27' }, callId: ToolCallId('mcp-positive'), signal: new AbortController().signal })
  assert.equal(echo.isError, false)
  assert(JSON.stringify(echo.content).includes('MCP-CHECK-27'))
  const denied = await ctx.tools.execute({ agent: resumed.agent, name: 'mcp__fixture__unselected', arguments: { text: 'must not run' }, callId: ToolCallId('mcp-negative'), signal: new AbortController().signal })
  assert.equal(denied.isError, true)
  await ctx.rainyExtensions.select(id, { skills: [], servers: [] })
  if (process.env.RAINY_TEST_IDA === '1') {
    await ctx.rainyExtensions.setIda(id, true)
    const idaAssembly = await resumed.agent.ctx.systemPrompt.assemble(assembleContextFor(resumed.agent))
    assert.equal(idaAssembly.tools.filter(tool => tool.name.startsWith('mcp__ida__')).length, 6)
    assert.equal((await ctx.rainyExtensions.catalog(id)).selection.ida, true)
    const result = await ctx.tools.execute({ agent: resumed.agent, name: 'mcp__ida__list_databases', arguments: {}, callId: ToolCallId('ida-gui'), signal: AbortSignal.timeout(30000) })
    assert.equal(result.isError, false)
    assert(JSON.stringify(result.content).includes('gui'), 'The official MCP must see the test database opened in the IDA GUI')
    const isolated = await other.agent.ctx.systemPrompt.assemble(assembleContextFor(other.agent))
    assert.deepEqual(isolated.tools.map(tool => tool.name).sort(), ['bash', 'edit', 'read', 'write'])
    await ctx.rainyExtensions.setIda(id, false)
    const removed = await resumed.agent.ctx.systemPrompt.assemble(assembleContextFor(resumed.agent))
    assert(!removed.tools.some(tool => tool.name.startsWith('mcp__ida__')))
    report.officialIda = { handshake: 'passed', guiDatabase: 'visible', sessionIsolation: 'passed', disable: 'passed',
      coreSchemaEstimate: estimateRequest({ messages: [], tools: isolated.tools }),
      enabledSchemaEstimate: estimateRequest({ messages: [], tools: idaAssembly.tools }) }
  }
  await other.dispose()
  report.mcpSelectionAndIsolation = 'passed'
  const cancel = new AbortController()
  const started = Date.now()
  const timer = setTimeout(() =>{  cancel.abort(new Error('fixture cancellation')) }, 350)
  const cancelled = await ctx.tools.execute({ agent: resumed.agent, name: shellTool, arguments: { command: process.platform === 'win32' ? 'Start-Sleep -Seconds 30' : 'sleep 30', description: 'Exercise cancellation of a sleeping command' }, callId: ToolCallId('cancel-command'), signal: cancel.signal })
  clearTimeout(timer)
  assert(cancel.signal.aborted, 'The command must reach cancellation, not fail before it starts')
  assert(Date.now() - started < 5000, 'Cancellation must settle promptly')
  assert.equal(cancelled.isError, true)
  report.commandCancellation = 'passed'
  const invalid = await ctx.tools.execute({ agent: resumed.agent, name: 'read', arguments: {}, callId: ToolCallId('invalid-arguments'), signal: new AbortController().signal })
  assert.equal(invalid.isError, true)
  report.invalidToolArguments = 'passed'

  mode = 'summary-failure'; rejectedRequests = 0
  const beforeFailure = JSON.stringify(resumed.agent.session.deriveMessages())
  await assert.rejects(ctx.compaction.compactNow(resumed.agent, new AbortController().signal))
  assert.equal(JSON.stringify(resumed.agent.session.deriveMessages()), beforeFailure, 'Failed summary must preserve the entire active history')
  assert.equal(rejectedRequests, 1, 'A summary failure must not repeatedly call the provider')
  report.summaryFailurePreservesHistory = 'passed'

  mode = 'recovery-history'
  const recovery = await ctx.agents.create({ sessionId: SessionId('rainy-overflow'), meta: { cwd: project },
    agentOptions: { provider: 'local', model: 'rainy-fixture' } })
  recovery.agent.followup(createUserMessage({ source: { kind: 'user' }, content: [{ type: 'text', text: 'Read one fixture page.' }] }))
  await recovery.agent.whenIdle()
  assert.equal(recoveryReads, 1)
  mode = 'overflow'; rejectedRequests = 0
  const overflowInput = createUserMessage({ content: [{ type: 'text', text: 'Continue after the injected provider overflow.' }], source: { kind: 'user' } })
  recovery.agent.followup(overflowInput)
  await recovery.agent.whenIdle()
  assert.equal(rejectedRequests, 2, 'Provider overflow may cause exactly one compressed retry')
  assert.deepEqual(recovery.agent.session.deriveMessages().filter(message => message.id === overflowInput.id), [overflowInput])
  report.overflowAttempts = rejectedRequests
  await recovery.dispose()

  mode = 'normal'
  const huge = await ctx.agents.create({ sessionId: SessionId('rainy-oversized'), meta: { cwd: project }, agentOptions: { provider: 'local', model: 'rainy-fixture' } })
  const dispatched = requests
  huge.agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Explicit user constraint: ' + '中'.repeat(contextWindow) }], source: { kind: 'user' } }))
  await huge.agent.whenIdle()
  assert.equal(requests, dispatched, 'An oversized new instruction must never be sent to a provider')
  assert(JSON.stringify(huge.agent.session.deriveMessages()).includes('中'.repeat(contextWindow)), 'User constraints must not be silently truncated')
  report.oversizedInputNoDispatch = 'passed'
  await huge.dispose()

  mode = 'disconnect'; rejectedRequests = 0
  const broken = await ctx.agents.create({ sessionId: SessionId('rainy-disconnected'), meta: { cwd: project }, agentOptions: { provider: 'local', model: 'rainy-fixture' } })
  broken.agent.followup(createUserMessage({ content: [{ type: 'text', text: 'Exercise a truncated streaming response.' }], source: { kind: 'user' } }))
  await broken.agent.whenIdle()
  assert(rejectedRequests >= 1 && rejectedRequests <= 2, 'A broken stream must settle within the configured retry limit')
  report.disconnectedStreamAttempts = rejectedRequests
  await broken.dispose()
  await promisify(execFile)('git', ['init', project])
  await writeFile(join(project, 'change.txt'), 'before\n')
  await promisify(execFile)('git', ['-C', project, 'add', 'change.txt'])
  await writeFile(join(project, 'change.txt'), 'after\n')
  assert((await fileDiff(project, 'change.txt')).includes('+after'))
  assert((await fileDiff(project, 'sample.txt')).includes('+fixture-line-0'))
  await writeFile(join(temporary, 'outside.txt'), 'outside')
  await assert.rejects(fileDiff(project, '../outside.txt'), /当前项目/)
  report.fileDiffAndBoundary = 'passed'
  await resumed.dispose()
  stopObserving()
  await writeFile(join(temporary, 'report.json'), JSON.stringify(report, null, 2))
  console.log(JSON.stringify({ ...report, reportPath: join(temporary, 'report.json') }))
} finally {
  await shutdown.shutdown(0)
  server.closeAllConnections()
  await new Promise<void>(resolve => server.close(() =>{  resolve() }))
}
