/** Commands that keep running after their tool call: the registry, the shell tool's background mode and its two tools. */
import { mkdtemp, realpath, rm } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, expect, it } from 'vitest'
import { Observations, type ToolContext } from '../../src/host/agent/tools/common.ts'
import { BackgroundJobs } from '../../src/host/agent/tools/jobs.ts'
import { commandOutputTool, shellTool, stopCommandTool } from '../../src/host/agent/tools/shell.ts'
import { decide } from '../../src/host/agent/permissions.ts'

const shell = process.platform === 'win32' ? 'pwsh' : 'bash'
const directories: string[] = []
const registries: BackgroundJobs[] = []
afterEach(async () => {
  for (const jobs of registries.splice(0)) await jobs.close()
  // Windows can hold a directory for a moment after the processes that ran in it exit.
  for (const directory of directories.splice(0)) await rm(directory, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })
})

async function context(): Promise<ToolContext & { jobs: BackgroundJobs }> {
  const cwd = await realpath(await mkdtemp(join(tmpdir(), 'rainy-bg-')))
  directories.push(cwd)
  const jobs = new BackgroundJobs()
  registries.push(jobs)
  return {
    cwd, sessionId: 'background-test', runtimeEnvironment: () => ({ environment: {}, executables: {} }), spillDir: join(cwd, 'spill'),
    toolTokens: () => 4000, observations: new Observations(), jobs,
  }
}

const text = (result: { content: { type: string; text?: string }[] }): string =>
  result.content.flatMap(block => block.type === 'text' && block.text !== undefined ? [block.text] : []).join('\n')

/** A command that prints a line, waits, then prints another, in either shell. */
function slowPrinter(): string {
  return shell === 'pwsh'
    ? 'Write-Output "first"; Start-Sleep -Milliseconds 1500; Write-Output "second"; Start-Sleep -Seconds 30'
    : 'echo first; sleep 1.5; echo second; sleep 30'
}

it('starts a command in the background, reads its new output once, and stops it', async () => {
  const tools = await context()
  const run = shellTool(tools, shell)
  const started = await run.execute('call-1', { command: slowPrinter(), description: 'Print two lines', background: true }, undefined, undefined)
  const id = started.details.jobId
  expect(id).toBe('cmd-1')
  expect(text(started)).toContain('is running in the background')
  expect(text(started)).toContain('first')
  expect(tools.jobs.list()).toMatchObject([{ id: 'cmd-1', state: 'running', description: 'Print two lines' }])

  // The second line arrives after the call returned; reading takes it once and does not repeat the first.
  await new Promise(done => setTimeout(done, 2500))
  const output = await commandOutputTool(tools).execute('call-2', { id: 'cmd-1' }, undefined, undefined)
  expect(text(output)).toContain('second')
  expect(text(output)).not.toContain('first')
  expect(text(output)).toContain('[cmd-1 is still running]')
  const again = await commandOutputTool(tools).execute('call-3', { id: 'cmd-1' }, undefined, undefined)
  expect(text(again)).toContain('(no new output)')

  const stopped = await stopCommandTool(tools).execute('call-4', { id: 'cmd-1' }, undefined, undefined)
  expect(text(stopped)).toContain('[cmd-1 stopped]')
  expect(stopped.details.state).toBe('exited')
  expect(tools.jobs.list()).toEqual([])
}, 30_000)

it('reports a background command that ended, and forgets it once its last output is read', async () => {
  const tools = await context()
  const run = shellTool(tools, shell)
  const started = await run.execute('call-1', { command: 'rainy-no-such-program-here', description: 'Run a missing program', background: true }, undefined, undefined)
  if (text(started).includes('exited')) {
    // It was already gone when the call returned, so nothing is left to read.
    expect(tools.jobs.list()).toEqual([])
    await expect(commandOutputTool(tools).execute('call-2', { id: 'cmd-1' }, undefined, undefined)).rejects.toThrow('unknown background command')
    return
  }
  // A loaded machine can take longer than the start watches; the next read reports the exit instead.
  for (let waited = 0; waited < 20_000 && tools.jobs.list()[0]?.state === 'running'; waited += 200) {
    await new Promise(done => setTimeout(done, 200))
  }
  const output = await commandOutputTool(tools).execute('call-2', { id: 'cmd-1' }, undefined, undefined)
  expect(text(output)).toMatch(/exited with code|was stopped on signal/u)
  expect(tools.jobs.list()).toEqual([])
}, 40_000)

it('refuses a ninth running command and names the tool that stops one', async () => {
  const tools = await context()
  const waiting = { argv: argvFor(), cwd: tools.cwd, env: {}, command: 'wait', description: 'Wait' }
  for (let index = 0; index < 8; index++) tools.jobs.start(waiting)
  expect(tools.jobs.list().filter(job => job.state === 'running')).toHaveLength(8)
  expect(() => tools.jobs.start(waiting)).toThrow('stop_command')
}, 30_000)

/** A process that just stays alive, started without a shell so the test does not pay for eight shells. */
function argvFor(): string[] {
  return [process.execPath, '-e', 'setTimeout(() => undefined, 60_000)']
}

it('closes every job with the chat', async () => {
  const tools = await context()
  tools.jobs.start({ argv: argvFor(), cwd: tools.cwd, env: {}, command: 'wait', description: 'Wait' })
  expect(tools.jobs.list()).toHaveLength(1)
  await tools.jobs.close()
  expect(tools.jobs.list()).toEqual([])
}, 30_000)

it('runs the reading and stopping tools in every permission mode, while the command itself still asks', async () => {
  const base = { cwd: '/p', roots: () => Promise.resolve(['/p']), rules: { commands: [], tools: [] }, readOnlyTools: new Set<string>(), canRemember: true, windows: false }
  for (const mode of ['read-only', 'ask'] as const) {
    expect(await decide({ ...base, mode, toolName: 'command_output', args: { id: 'cmd-1' } })).toEqual({ kind: 'allow' })
    expect(await decide({ ...base, mode, toolName: 'stop_command', args: { id: 'cmd-1' } })).toEqual({ kind: 'allow' })
  }
  const started = await decide({ ...base, mode: 'ask', toolName: 'bash', args: { command: 'npm run dev', background: true } })
  expect(started.kind).toBe('ask')
})
