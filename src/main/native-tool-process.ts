/** Start Windows tools with their bundled dependencies and an explicit working directory. */
import { execFile, spawn } from 'node:child_process'
import { mkdir } from 'node:fs/promises'
import { dirname, delimiter, join } from 'node:path'
import { promisify } from 'node:util'
import type { NativeInvocation } from './native-tools.ts'
import { quotePowerShell, windowsPowerShellPath } from './powershell.ts'

const exec = promisify(execFile)

/**
 * Keep Windows process essentials while withholding inherited service credentials.
 * @param source - carrier environment.
 * @param invocation - checked installed command.
 * @returns an invocation-local environment whose PATH contains bundled binaries and Windows utilities.
 */
export function nativeToolEnvironment(source: NodeJS.ProcessEnv, invocation: NativeInvocation): NodeJS.ProcessEnv {
  const environment: NodeJS.ProcessEnv = {}
  const windowsRoot = source.SystemRoot ?? source.SYSTEMROOT ?? 'C:\\Windows'
  for (const [name, value] of Object.entries(source)) {
    if (/key|secret|token|password|credential/i.test(name) || /^RAINY_|^DEEPSEEK_|^OPENAI_|^ANTHROPIC_|^ELECTRON_/i.test(name)) continue
    if (name.toLowerCase() === 'path') continue
    if (/^(?:JAVA_HOME|JDK_HOME|JRE_HOME|PYTHON.*|VIRTUAL_ENV|CONDA_.*|NODE_OPTIONS|NODE_PATH|DOTNET_.*)$/i.test(name)
      || /^(?:_JAVA_OPTIONS|JAVA_TOOL_OPTIONS|JDK_JAVA_OPTIONS|CLASSPATH|PSModulePath|PERL5LIB|PERL5OPT|PERLLIB)$/i.test(name)) continue
    if (/^(?:QT_PLUGIN_PATH|QT_QPA_PLATFORM_PLUGIN_PATH|QML2?_IMPORT_PATH|QTDIR)$/i.test(name)) continue
    if (/^(?:IDAUSR|IDADIR|IDAPYTHON_.*|TESSDATA_PREFIX|MAGICK_.*)$/i.test(name)
      || /^(?:GIMP3?_.*|GI_TYPELIB_PATH|GTK_PATH|GIO_EXTRA_MODULES)$/i.test(name)) continue
    environment[name] = value
  }
  environment.PATH = [...new Set([dirname(invocation.executable), dirname(invocation.target), invocation.cwd,
    ...invocation.pythonRoot ? [invocation.pythonRoot, join(invocation.pythonRoot, 'DLLs')] : [],
    ...invocation.dotnetRoot ? [invocation.dotnetRoot] : [],
    join(windowsRoot, 'System32'), windowsRoot, join(windowsRoot, 'System32/Wbem'),
    join(windowsRoot, 'System32/WindowsPowerShell/v1.0')])].join(delimiter)
  if (invocation.kind === 'java') environment.JAVA_HOME = dirname(dirname(invocation.executable))
  if (invocation.dotnetRoot) {
    environment.DOTNET_ROOT = invocation.dotnetRoot
    environment.DOTNET_ROOT_X64 = invocation.dotnetRoot
    environment.DOTNET_MULTILEVEL_LOOKUP = '0'
  }
  if (invocation.pythonRoot) {
    environment.PYTHONHOME = invocation.pythonRoot
    environment.PYTHONNOUSERSITE = '1'
    environment.PYTHONUTF8 = '1'
  }
  if (invocation.id === 'ida') environment.IDAUSR = invocation.userData
  return environment
}


/**
 * Keep the tool's console open after its initial help or command has completed.
 * @param invocation - checked installed command; no renderer command text is accepted.
 * @returns an encoded PowerShell command with literal path and argument values.
 */
export function nativeConsoleCommand(invocation: NativeInvocation): string {
  const command = `$ErrorActionPreference = 'Continue'\nSet-Location -LiteralPath ${quotePowerShell(invocation.cwd)}\n& ${quotePowerShell(invocation.executable)} ${invocation.args.map(quotePowerShell).join(' ')}\n`
  return Buffer.from(command, 'utf16le').toString('base64')
}

/**
 * Request a separate Windows console without inheriting the GUI carrier's redirected handles.
 * @param invocation - checked installed command.
 * @param powershell - absolute system PowerShell executable.
 * @returns the encoded command for the hidden launcher.
 */
export function nativeConsoleLauncher(invocation: NativeInvocation, powershell: string): string {
  const command = `$ErrorActionPreference = 'Stop'\nStart-Process -FilePath ${quotePowerShell(powershell)} -WorkingDirectory ${quotePowerShell(invocation.cwd)} -ArgumentList @('-NoLogo', '-NoProfile', '-NoExit', '-EncodedCommand', '${nativeConsoleCommand(invocation)}') -ErrorAction Stop\n`
  return Buffer.from(command, 'utf16le').toString('base64')
}

/**
 * Open a native GUI or an interactive Windows console.
 * @param invocation - absolute paths resolved inside the installed tool pack.
 * @param environment - carrier environment from which secrets are removed.
 * @returns completion when the operating system accepts the launch request; the window belongs to the user.
 */
export async function startNativeProcess(invocation: NativeInvocation, environment: NodeJS.ProcessEnv): Promise<void> {
  if (invocation.kind === 'web') throw new Error('网页工具需要使用独立网页窗口')
  const env = nativeToolEnvironment(environment, invocation)
  if (invocation.id === 'ida') await mkdir(invocation.userData, { recursive: true })
  if (invocation.kind === 'console') {
    const powershell = windowsPowerShellPath(environment)
    await exec(powershell, ['-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', nativeConsoleLauncher(invocation, powershell)],
      { cwd: invocation.cwd, windowsHide: true, timeout: 30_000, env })
    return
  }
  // windowsHide requests SW_HIDE, which many GUI tools honor for their first window and then never show.
  const child = spawn(invocation.executable, [...invocation.args], { cwd: invocation.cwd, shell: false, detached: true,
    windowsHide: false, stdio: 'ignore', env })
  await new Promise<void>((resolve, reject) => { child.once('spawn', () => { resolve() }); child.once('error', reject) })
  if (child.pid === undefined) throw new Error('Windows 未返回工具进程编号')
  child.unref()
}
