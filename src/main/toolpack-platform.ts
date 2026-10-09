/** Windows process and filesystem operations for the native-tool installer. */
import { execFile } from 'node:child_process'
import { join } from 'node:path'
import { promisify } from 'node:util'
import { z } from 'zod'
import { ToolPackInstallError } from './toolpack-format.ts'
import type { ToolPackPlatform, ToolPackUnit } from './toolpack-format.ts'
import { renameToolPackPath, toolPackChild } from './toolpack-files.ts'
import { toolPackFileSystem } from './toolpack-fs.ts'
import { windowsPowerShellPath } from './powershell.ts'

const exec = promisify(execFile)
const { statfs } = toolPackFileSystem.promises
const processSchema = z.array(z.object({ pid: z.number(), executable: z.string(), commandLine: z.string() }))

/** A process observation returned by the Windows management API. */
export type ToolPackProcess = z.infer<typeof processSchema>[number]

/** Locate an active tool, retained PowerShell console, or older RainyAgent process.
 * @param processes Validated Windows process observations.
 * @param installRoot Directory being updated.
 * @param units Tool directories included in the update.
 * @param ownPid Maintenance process to exclude.
 * @returns The blocking process, without executing any command-line content.
 */
export function findBusyToolPackProcess(
  processes: ToolPackProcess[], installRoot: string, units: ToolPackUnit[], ownPid: number,
): ToolPackProcess | undefined {
  const normalize = (path: string): string => path.replaceAll('/', '\\').toLowerCase()
  const roots = units.filter(unit => unit.kind === 'directory').map(unit => normalize(toolPackChild(installRoot, unit.path)))
  const application = normalize(join(installRoot, 'RainyAgent.exe'))
  return processes.find((item) => {
    if (item.pid === ownPid) return false
    const executable = normalize(item.executable)
    if (executable === application && !/--type=/.test(item.commandLine)) return true
    const encoded = /-(?:encodedcommand|enc|ec)\s+["']?([a-z0-9+/=]+)/i.exec(item.commandLine)?.[1]
    const decoded = encoded ? Buffer.from(encoded, 'base64').toString('utf16le').replace(/(['\u2018-\u201b])\1/g, '$1') : ''
    const command = normalize(item.commandLine + '\n' + decoded)
    return roots.some(root => executable.startsWith(root + '\\') || command.includes(root + '\\'))
  })
}

async function assertNotBusy(installRoot: string, units: ToolPackUnit[]): Promise<void> {
  if (process.platform !== 'win32') return
  const command = '[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); @(Get-CimInstance Win32_Process | ForEach-Object { @{ pid = [int]$_.ProcessId; executable = [string]$_.ExecutablePath; commandLine = [string]$_.CommandLine } }) | ConvertTo-Json -Compress'
  const powershell = windowsPowerShellPath()
  const result = await exec(powershell, ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', Buffer.from(command, 'utf16le').toString('base64')], {
    windowsHide: true, timeout: 30_000, maxBuffer: 4 * 1024 * 1024,
    env: Object.fromEntries(Object.entries(process.env).filter(([name]) => !/KEY|SECRET|TOKEN|PASSWORD|^PSModulePath$/i.test(name))),
  })
  const processes = processSchema.parse(JSON.parse(result.stdout))
  const busy = findBusyToolPackProcess(processes, installRoot, units, process.pid)
  if (busy) throw new ToolPackInstallError('tools-busy', `工具仍在运行（进程 ${busy.pid}）。请保存工作并手动关闭该工具，再重试安装。`)
}

/** Default operating-system calls; tests inject failures without changing host resources. */
export const nativeToolPackPlatform: ToolPackPlatform = {
  async availableBytes(path) { const space = await statfs(path); return space.bavail * space.bsize },
  assertNotBusy,
  move: renameToolPackPath,
}
