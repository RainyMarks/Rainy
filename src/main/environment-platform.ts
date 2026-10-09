/** Windows implementation of read-only preflight and user-requested offline setup. */
import { execFile } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createReadStream } from 'node:fs'
import { lstat, mkdir, readFile, readdir, stat } from 'node:fs/promises'
import { createServer } from 'node:net'
import { arch, platform, release } from 'node:os'
import { join, resolve } from 'node:path'
import { promisify } from 'node:util'
import { z } from 'zod'
import { environmentMedia } from './environment-media.ts'
import { EnvironmentSetupError, environmentStateDirectory, writeEnvironmentRecord } from './environment.ts'
import type { EnvironmentPlatform, EnvironmentSystem } from './environment.ts'
import { quotePowerShell, windowsPowerShellPath } from './powershell.ts'

const execFileAsync = promisify(execFile)
const distributionSchema = z.object({ name: z.string(), version: z.number(), basePath: z.string() })
const systemSchema = z.object({
  virtualization: z.boolean(), componentsEnabled: z.boolean(), bootId: z.string(), distributions: z.array(distributionSchema),
})
const ownerSchema = z.object({ version: z.literal(1), installRoot: z.string(), distro: z.string(), imageSha256: z.string() }).strict()


function sanitizedEnvironment(): NodeJS.ProcessEnv {
  return Object.fromEntries(Object.entries(process.env).filter(([name]) => !/KEY|SECRET|TOKEN|PASSWORD|^PSModulePath$/i.test(name)))
}

async function run(file: string, args: string[], timeout = 60_000): Promise<string> {
  try {
    const result = await execFileAsync(file, args, { windowsHide: true, encoding: 'utf8', maxBuffer: 2 * 1024 * 1024, timeout, env: sanitizedEnvironment() })
    return result.stdout.replaceAll('\0', '').trim()
  } catch (error) {
    const detail = error instanceof Error ? error.message : '未知错误'
    throw new EnvironmentSetupError('command-failed', `${file} 执行失败：${detail}`)
  }
}

async function powershell(script: string, timeout?: number): Promise<string> {
  return run(windowsPowerShellPath(), ['-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', Buffer.from('$ErrorActionPreference = "Stop"\n[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n' + script, 'utf16le').toString('base64')], timeout)
}

async function exists(path: string): Promise<boolean> {
  try { await stat(path); return true } catch (error) {
    if (error instanceof Error && 'code' in error && error.code === 'ENOENT') return false
    throw error
  }
}

async function verifyMedia(path: string, expected: { sha256: string; bytes: number }): Promise<void> {
  let size: number
  try { size = (await stat(path)).size } catch (error) { throw new EnvironmentSetupError('media-missing', `离线安装文件缺失：${path}。请重新安装完整安装包。${error instanceof Error ? ' ' + error.message : ''}`) }
  if (size !== expected.bytes) throw new EnvironmentSetupError('media-integrity', `安装文件大小不匹配：${path}。请重新安装完整安装包。`)
  const hash = createHash('sha256')
  for await (const chunk of createReadStream(path)) {
    if (!Buffer.isBuffer(chunk)) throw new EnvironmentSetupError('media-integrity', `安装文件读取未返回二进制数据：${path}`)
    hash.update(chunk)
  }
  if (hash.digest('hex') !== expected.sha256) throw new EnvironmentSetupError('media-integrity', `安装文件校验失败：${path}。请重新安装完整安装包。`)
}

function normalWindowsPath(path: string): string { return resolve(path.replace(/^\\\\\?\\/, '')).toLowerCase() }

/** Create Windows operations without running commands or changing the machine.
 * @param options Installation and private desktop-data paths.
 * @returns Operations called by the setup controller after its action checks.
 */
export function createWindowsEnvironmentPlatform(
  options: { installRoot: string; userData: string; mediaRoot?: string; resolveMediaRoot?: (() => Promise<string>) | undefined },
): EnvironmentPlatform {
  const installRoot = resolve(options.installRoot)
  const mediaRoot = options.mediaRoot ?? join(installRoot, 'resources', 'environment')
  const setupDirectory = environmentStateDirectory(installRoot, options.userData)
  const runtimeRoot = join(installRoot, 'runtime', 'wsl')
  const distroDirectory = join(runtimeRoot, 'data')
  const ownerPath = join(runtimeRoot, 'owner.json')
  const lockId = createHash('sha256').update(installRoot.toLowerCase()).digest('hex')

  async function inspectSystem(): Promise<EnvironmentSystem> {
    const build = Number(release().split('.')[2] ?? 0)
    if (platform() !== 'win32' || arch() !== 'x64' || build < 19045) return { supported: false, reason: '需要 Windows 10 22H2 / Windows 11 的 x64 系统。', virtualization: false, wslInstalled: false, componentsEnabled: false, bootId: '', distributions: [] }
    const result = systemSchema.parse(JSON.parse(await powershell(`
$computer = Get-CimInstance Win32_ComputerSystem
$processor = @(Get-CimInstance Win32_Processor)
$features = @(Get-CimInstance Win32_OptionalFeature | Where-Object { $_.Name -eq 'VirtualMachinePlatform' })
$enabled = @($features | Where-Object { $_.InstallState -eq 1 }).Count -eq 1
$boot = (Get-CimInstance Win32_OperatingSystem).LastBootUpTime.ToUniversalTime().ToString('o')
$distros = @()
$lxss = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Lxss'
if (Test-Path -LiteralPath $lxss) {
  $distros = @(Get-ChildItem -LiteralPath $lxss | ForEach-Object {
    $item = Get-ItemProperty -LiteralPath $_.PSPath
    if ($item.DistributionName) { @{ name = [string]$item.DistributionName; version = [int]$item.Version; basePath = [string]$item.BasePath } }
  })
}
@{ virtualization = [bool]($computer.HypervisorPresent -or @($processor | Where-Object { $_.VirtualizationFirmwareEnabled }).Count -gt 0); componentsEnabled = $enabled; bootId = $boot; distributions = $distros } | ConvertTo-Json -Depth 5 -Compress
`)))
    let wslInstalled = true
    try { await run('wsl.exe', ['--version'], 30_000) } catch (error) {
      if (!(error instanceof EnvironmentSetupError)) throw error
      wslInstalled = false
    }
    return { supported: true, ...result, wslInstalled }
  }

  async function checkDistribution(name: string): Promise<void> {
    try {
      const output = await run('wsl.exe', ['--distribution', name, '--exec', 'bash', '-c', "python3 -c 'import sys; assert sys.version_info >= (3,12), \"Python 3.12 or newer is required\"; print(\"RAINY_ENVIRONMENT_READY\")'"], 90_000)
      if (!output.includes('RAINY_ENVIRONMENT_READY')) throw new Error('未收到环境检查完成标记')
    } catch (error) { throw new EnvironmentSetupError('distro-unhealthy', `环境“${name}”无法运行 Bash / Python 3.12+。可修复原环境后重试，或创建专用环境。${error instanceof Error ? '\n' + error.message : ''}`) }
  }

  async function acquireLock(): Promise<() => Promise<void>> {
    await mkdir(setupDirectory, { recursive: true })
    const address = platform() === 'win32' ? `\\\\.\\pipe\\rainy-environment-${lockId}` : join(setupDirectory, 'setup.sock')
    const server = createServer(socket => socket.end())
    await new Promise<void>((accept, reject) => {
      server.once('error', (error) => { reject(new EnvironmentSetupError('setup-busy', `另一 RainyAgent 进程正在准备这个安装目录。请等待它完成后重试。${error.message}`)) })
      server.listen(address, () => { accept() })
    })
    return async () => {
      await new Promise<void>((accept, reject) => {
        server.close((error) => { if (error) reject(error); else accept() })
      })
    }
  }

  async function installSystem(progress: (message: string) => void): Promise<{ rebootRequired: boolean }> {
    const before = await inspectSystem()
    const msi = join(await options.resolveMediaRoot?.() ?? mediaRoot, environmentMedia.wsl.file)
    progress('正在校验随包提供的 WSL 安装文件…')
    await verifyMedia(msi, environmentMedia.wsl)
    await powershell(`$signature = Get-AuthenticodeSignature -LiteralPath ${quotePowerShell(msi)}\nif ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') { throw 'WSL Microsoft signature verification failed' }`)
    await mkdir(setupDirectory, { recursive: true })
    const reportPath = join(setupDirectory, 'system-install-result.json')
    const elevatedScript = `
$ErrorActionPreference = 'Stop'
$report = ${quotePowerShell(reportPath)}
try {
  $reboot = $false
  foreach ($feature in @('VirtualMachinePlatform')) {
    $state = Get-WindowsOptionalFeature -Online -FeatureName $feature
    if ($state.State -ne 'Enabled') {
      $result = Enable-WindowsOptionalFeature -Online -FeatureName $feature -All -NoRestart
      $reboot = $true
    }
  }
  if (${before.wslInstalled ? '$false' : '$true'}) {
    $msi = ${quotePowerShell(msi)}
    $install = Start-Process -FilePath 'msiexec.exe' -ArgumentList @('/i', ('"' + $msi + '"'), '/qn', '/norestart') -PassThru -Wait -WindowStyle Hidden
    if ($install.ExitCode -notin @(0, 3010)) { throw ('WSL installer exit code: ' + $install.ExitCode) }
    if ($install.ExitCode -eq 3010) { $reboot = $true }
  }
  @{ ok = $true; rebootRequired = $reboot } | ConvertTo-Json -Compress | Set-Content -LiteralPath $report -Encoding UTF8
} catch {
  @{ ok = $false; message = $_.Exception.Message } | ConvertTo-Json -Compress | Set-Content -LiteralPath $report -Encoding UTF8
  exit 1
}
`
    progress('正在请求管理员权限并安装 Windows 组件；请留意系统确认窗口…')
    const encoded = Buffer.from(elevatedScript, 'utf16le').toString('base64')
    await powershell(`$process = Start-Process -FilePath ${quotePowerShell(windowsPowerShellPath())} -ArgumentList @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', '${encoded}') -Verb RunAs -PassThru -Wait -WindowStyle Hidden\nif ($process.ExitCode -ne 0) { throw ${quotePowerShell(`系统组件安装失败，请检查 ${reportPath} 后重试。`)} }`, 0)
    const result = z.object({ ok: z.literal(true), rebootRequired: z.boolean() }).parse(JSON.parse((await readFile(reportPath, 'utf8')).replace(/^\uFEFF/, '')))
    return { rebootRequired: result.rebootRequired }
  }

  async function createManagedDistribution(name: string, progress: (message: string) => void): Promise<void> {
    const image = join(await options.resolveMediaRoot?.() ?? mediaRoot, environmentMedia.ubuntu.file)
    progress('正在校验 Ubuntu 离线镜像…')
    await verifyMedia(image, environmentMedia.ubuntu)
    for (const directory of [join(installRoot, 'runtime'), runtimeRoot]) {
      if (await exists(directory) && (await lstat(directory)).isSymbolicLink()) throw new EnvironmentSetupError('runtime-link', '环境存放目录不能是链接或 Windows 联接。')
    }
    if (await exists(distroDirectory) && (await lstat(distroDirectory)).isSymbolicLink()) throw new EnvironmentSetupError('runtime-link', '专用环境目录不能是链接或 Windows 联接。')
    await mkdir(runtimeRoot, { recursive: true })
    const ownerExists = await exists(ownerPath)
    if (ownerExists) {
      const owner = ownerSchema.parse(JSON.parse(await readFile(ownerPath, 'utf8')))
      if (owner.installRoot !== installRoot || owner.distro !== name || owner.imageSha256 !== environmentMedia.ubuntu.sha256) throw new EnvironmentSetupError('owner-mismatch', '专用环境归属记录不匹配，已保留原有文件。')
    } else {
      if (await exists(distroDirectory) && (await readdir(distroDirectory)).length > 0) throw new EnvironmentSetupError('unowned-runtime', 'runtime/wsl/data 已有未标记的文件，已保留原有内容。请移走该目录后重试。')
      await writeEnvironmentRecord(ownerPath, { version: 1, installRoot, distro: name, imageSha256: environmentMedia.ubuntu.sha256 })
    }
    const registered = (await inspectSystem()).distributions.find(distro => distro.name === name)
    if (registered && (registered.version !== 2 || normalWindowsPath(registered.basePath) !== normalWindowsPath(distroDirectory))) throw new EnvironmentSetupError('distro-name-conflict', `发行版“${name}”已被其他目录使用，已保留原有环境。`)
    if (!registered) {
      const vhd = join(distroDirectory, 'ext4.vhdx')
      if (await exists(vhd)) {
        progress('正在重新连接上次创建的专用环境…')
        await run('wsl.exe', ['--import-in-place', name, vhd], 0)
      } else {
        progress('正在创建 RainyAgent 专用 Ubuntu 环境，请稍候…')
        await mkdir(distroDirectory, { recursive: true })
        await run('wsl.exe', ['--import', name, distroDirectory, image, '--version', '2'], 0)
      }
    }
    progress('正在配置专用环境中的本地用户…')
    await run('wsl.exe', ['--distribution', name, '--user', 'root', '--exec', 'bash', '-c', "set -eu; if ! id rainy >/dev/null 2>&1; then useradd --create-home --shell /bin/bash rainy; fi; python3 -c 'import configparser, pathlib, sys; assert sys.version_info >= (3,12); p=pathlib.Path(\"/etc/wsl.conf\"); c=configparser.ConfigParser(); c.read(p); c[\"user\"]={\"default\":\"rainy\"}; f=p.open(\"w\"); c.write(f); f.close()'"], 90_000)
    await run('wsl.exe', ['--terminate', name], 60_000)
    const defaultUser = await run('wsl.exe', ['--distribution', name, '--exec', 'id', '-un'], 90_000)
    if (defaultUser !== 'rainy') throw new EnvironmentSetupError('default-user', '专用环境默认用户设置尚未生效，请重试恢复。')
    await checkDistribution(name)
  }

  return { inspectSystem, checkDistribution, acquireLock, installSystem, createManagedDistribution }
}
