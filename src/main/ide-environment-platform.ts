/** Windows WSL commands for readonly development checks and explicit offline package installation. */
import { spawn } from 'node:child_process'
import { posix } from 'node:path'
import { StringDecoder } from 'node:string_decoder'
import { z } from 'zod'
import { IdeEnvironmentError, supportsIdeOfflineInstall } from './ide-environment.ts'
import type { IdeEnvironmentConfig, IdeEnvironmentInspection, IdeEnvironmentPlatform } from './ide-environment.ts'

const inspectionSchema = z.object({
  os: z.string(), osVersion: z.string(), architecture: z.string(), mediaPresent: z.boolean(), mediaMatches: z.boolean(),
  packageCount: z.number().int().nonnegative(), incompletePackages: z.array(z.string()),
  tools: z.array(z.object({ name: z.string(), path: z.string().nullable(), ready: z.boolean() }).strict()),
}).strict()

const probe = `import json, platform, shutil, subprocess, sys
from pathlib import Path
root = Path(sys.argv[1])
release = dict(line.split('=', 1) for line in Path('/etc/os-release').read_text().splitlines() if '=' in line)
os_id = release.get('ID', '').strip('"')
os_version = release.get('VERSION_ID', '').strip('"')
architecture = platform.machine()
architecture = 'amd64' if architecture == 'x86_64' else architecture
manifest_path = root / 'system-packages' / 'manifest.json'
manifest = json.loads(manifest_path.read_text()) if manifest_path.is_file() else None
media = manifest is not None and (root / 'install-system-packages.py').is_file()
matches = media and manifest.get('version') == 1 and manifest.get('os') == os_id and manifest.get('osVersion') == os_version and manifest.get('architecture') == architecture
packages = [row['name'] for row in manifest['packages']] if manifest is not None else []
incomplete = []
if matches:
    for name in packages:
        result = subprocess.run(['dpkg-query', '-W', '-f=' + chr(36) + '{Status}', '--', name], capture_output=True, text=True)
        if result.returncode or result.stdout.strip() != 'install ok installed': incomplete.append(name)
tools = []
for name in ['python3', 'gcc', 'g++', 'gdb', 'cmake', 'ninja', 'clangd', 'clang-format', 'php']:
    path = shutil.which(name)
    tools.append({'name': name, 'path': path, 'ready': path is not None})
print(json.dumps({'os': os_id, 'osVersion': os_version, 'architecture': architecture, 'mediaPresent': bool(media), 'mediaMatches': bool(matches), 'packageCount': len(packages), 'incompletePackages': incomplete, 'tools': tools}))
`

/** One shell-free WSL invocation; zero timeout is reserved for an admitted package-manager run. */
export interface IdeEnvironmentCommand {
  readonly args: readonly string[]
  readonly timeoutMs: number
  readonly onProgress?: (message: string) => void
}

/** Testable command owner; completion includes the child close event. */
export type IdeEnvironmentCommandRunner = (command: IdeEnvironmentCommand) => Promise<string>

/** Fixed selected distribution and trusted runtime resources, never supplied by the setup page. */
export interface WindowsIdeEnvironmentOptions {
  readonly distro: string
  readonly resourceRoot: string
  readonly config: IdeEnvironmentConfig
  readonly execute?: IdeEnvironmentCommandRunner
}

function run(command: IdeEnvironmentCommand, config: IdeEnvironmentConfig): Promise<string> {
  return new Promise((accept, reject) => {
    const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !/KEY|TOKEN|PASSWORD|SECRET|^PSModulePath$/iu.test(key)))
    const child = spawn('wsl.exe', [...command.args], { windowsHide: true, env, stdio: ['ignore', 'pipe', 'pipe'] })
    let output = ''
    let diagnostic = ''
    let spawnError: Error | undefined
    let timedOut = false
    const stdout = new StringDecoder('utf8')
    const stderr = new StringDecoder('utf8')
    const publish = (text: string): void => {
      if (text.trim() === '') return
      try { command.onProgress?.(text) }
      catch (error) { console.error('IDE package progress listener failed', error) }
    }
    child.stdout.on('data', (chunk: Buffer) => {
      const text = stdout.write(chunk)
      output = (output + text).slice(-config.maxOutputCharacters)
      publish(text)
    })
    child.stderr.on('data', (chunk: Buffer) => {
      const text = stderr.write(chunk)
      diagnostic = (diagnostic + text).slice(-config.maxOutputCharacters)
      publish(text)
    })
    child.once('error', (error) => { spawnError = error })
    const timer = command.timeoutMs === 0 ? undefined : setTimeout(() => { timedOut = true; child.kill() }, command.timeoutMs)
    timer?.unref()
    child.once('close', (code, signal) => {
      if (timer !== undefined) clearTimeout(timer)
      output = (output + stdout.end()).slice(-config.maxOutputCharacters)
      diagnostic = (diagnostic + stderr.end()).slice(-config.maxOutputCharacters)
      if (spawnError !== undefined) { reject(new IdeEnvironmentError('wsl-command-failed', spawnError.message)); return }
      if (timedOut) { reject(new IdeEnvironmentError('development-check-timeout', '开发环境检查超时，请确认所选 WSL 环境可以启动后重试。')); return }
      if (code !== 0 || signal !== null) {
        reject(new IdeEnvironmentError('development-command-failed',
          `开发工具命令未完成（退出码 ${code ?? '无'}${signal === null ? '' : `，信号 ${signal}`}）。\n${diagnostic || output}`))
        return
      }
      accept(output.replaceAll('\0', '').trim())
    })
  })
}

/**
 * Create readonly probes and the selected-distro installer without launching a command.
 * @param options - fixed distribution, POSIX runtime resource directory and budgets.
 * @returns explicitly invoked operations; only install requests root.
 */
export function createWindowsIdeEnvironmentPlatform(options: WindowsIdeEnvironmentOptions): IdeEnvironmentPlatform {
  if (!options.distro.trim() || options.distro.includes('\0') || !posix.isAbsolute(options.resourceRoot) || options.resourceRoot.includes('\0')) {
    throw new IdeEnvironmentError('development-path-invalid', '所选环境或开发工具资源目录无效。')
  }
  const execute = options.execute ?? (command => run(command, options.config))
  const inspect = async (): Promise<IdeEnvironmentInspection> => {
    const output = await execute({ args: ['-d', options.distro, '--exec', 'python3', '-c', probe, options.resourceRoot],
      timeoutMs: options.config.inspectTimeoutMs })
    try { return inspectionSchema.parse(JSON.parse(output)) }
    catch (_invalidProbeResult) { throw new IdeEnvironmentError('development-check-invalid', '未能读取完整的开发环境检查结果，请重新检查。') }
  }
  return { inspect, install: async (onProgress) => {
    const before = await inspect()
    if (!supportsIdeOfflineInstall(before)) throw new IdeEnvironmentError('unsupported-distribution', '离线开发组件只支持 Ubuntu 26.04 amd64，未修改所选环境。')
    if (!before.mediaPresent || !before.mediaMatches || before.packageCount === 0) {
      throw new IdeEnvironmentError('development-media-missing', '离线开发组件缺失或不匹配，请重新安装完整的 RainyAgent 安装包。')
    }
    const output = await execute({
      args: ['-d', options.distro, '-u', 'root', '--exec', 'python3', posix.join(options.resourceRoot, 'install-system-packages.py'),
        posix.join(options.resourceRoot, 'system-packages')], timeoutMs: 0, onProgress,
    })
    const last = output.split(/\r?\n/u).findLast(line => line.trim() !== '')
    try {
      const result = z.object({ installedPackages: z.number().int().positive(), offline: z.literal(true) }).strict().parse(JSON.parse(last ?? ''))
      if (result.installedPackages !== before.packageCount) throw new Error('Package count differs.')
    } catch (_invalidInstallResult) {
      throw new IdeEnvironmentError('development-install-unconfirmed', '未收到离线安装完成确认。请查看安装详情并重新检查环境。')
    }
  } }
}
