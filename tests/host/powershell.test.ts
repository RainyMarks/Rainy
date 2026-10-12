import { mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { buildSystemPrompt } from '../../src/host/agent/prompt.ts'
import { Observations } from '../../src/host/agent/tools/common.ts'
import { shellTool } from '../../src/host/agent/tools/shell.ts'
import { resolvePowerShell } from '../../src/host/process.ts'

const directories: string[] = []
afterEach(async () => { for (const directory of directories.splice(0)) await rm(directory, { recursive: true, force: true }) })

async function scratch(): Promise<string> {
  const directory = await realpath(await mkdtemp(join(tmpdir(), 'rainy-pwsh-')))
  directories.push(directory)
  return directory
}

async function touch(path: string): Promise<string> {
  await mkdir(join(path, '..'), { recursive: true })
  await writeFile(path, '')
  return path
}

describe('resolvePowerShell', () => {
  it('prefers an explicit path, then the basic component, Program Files, PATH and finally Windows PowerShell 5.1', async () => {
    const root = await scratch()
    const env = { ProgramFiles: join(root, 'Program Files'), SystemRoot: join(root, 'Windows'), PATH: join(root, 'bin') }
    const desktop = join(root, 'Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe')
    expect(resolvePowerShell({ env })).toEqual({ path: desktop, edition: 'desktop' })
    const onPath = await touch(join(root, 'bin', 'pwsh.exe'))
    expect(resolvePowerShell({ env })).toEqual({ path: onPath, edition: 'core' })
    const installed = await touch(join(root, 'Program Files', 'PowerShell', '7', 'pwsh.exe'))
    expect(resolvePowerShell({ env }).path).toBe(installed)
    const component = await touch(join(root, 'components', 'windows-basic', 'pwsh', 'pwsh.exe'))
    expect(resolvePowerShell({ env, component }).path).toBe(component)
    const configured = await touch(join(root, 'tools', 'powershell.exe'))
    expect(resolvePowerShell({ env, component, configured })).toEqual({ path: configured, edition: 'desktop' })
    expect(resolvePowerShell({ env, component, configured: join(root, 'missing', 'pwsh.exe') }).path).toBe(component)
  })
})

describe('PowerShell edition in the system prompt', () => {
  const base = { cwd: 'C:\\work', platform: 'win32' as const, tools: ['pwsh'], mcpInstructions: [], extraRoots: [], skills: '', globalPrompt: '' }
  it('tells the model that Windows PowerShell 5.1 has no pipeline chain operators', () => {
    expect(buildSystemPrompt({ ...base, powershell: 'desktop' })).toContain('Windows PowerShell 5.1 on Windows, which has no && or || operators')
    expect(buildSystemPrompt({ ...base, powershell: 'core' })).toContain('Commands execute in PowerShell 7 on Windows.')
    expect(buildSystemPrompt(base)).toContain('Commands execute in PowerShell on Windows.')
  })
})

describe.runIf(process.platform === 'win32')('command tool on Windows PowerShell 5.1', () => {
  it('runs a command with UTF-8 output and reports the exit code', async () => {
    const cwd = await scratch()
    const tool = shellTool({
      cwd, sessionId: 'powershell-test', runtimeEnvironment: () => ({ environment: {}, executables: {} }), spillDir: join(cwd, 'spill'),
      toolTokens: () => 4000, observations: new Observations(),
      pwshPath: join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe'),
    }, 'pwsh')
    const result = await tool.execute('call-1', { command: 'Write-Output "雨 $($PSVersionTable.PSVersion.Major)"; exit 3', description: 'Print a marker' }, undefined, undefined)
    const text = result.content.flatMap(block => block.type === 'text' ? [block.text] : []).join('\n')
    expect(text).toContain('雨 5')
    expect(text).toContain('[exit code: 3]')
  }, 30_000)
})
