/** The Windows OpenSSH client for SSH execution targets: destinations, arguments, the remote Python loader and runs. */
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process'
import { existsSync } from 'node:fs'
import { createInterface } from 'node:readline'
import type { Readable } from 'node:stream'
import { join } from 'node:path'
import { localized } from '../shared/locale.ts'

/** Where `ssh` connects: a `~/.ssh/config` alias or `[user@]host`, and an optional port. */
export interface SshDestination { destination: string; port?: number | undefined }

/** `[user@]host` or an alias; never starting with `-`, so it cannot be read as an option. */
const DESTINATION = /^(?:[A-Za-z0-9_][A-Za-z0-9._%+-]*@)?(?:[A-Za-z0-9_][A-Za-z0-9._-]*|\[[0-9A-Fa-f:.]+\])$/u

/**
 * Validate what the user typed: `user@host`, `host`, `user@host:port` or a config alias.
 * @param text - destination text.
 * @param port - port from its own field, if any.
 * @returns the destination.
 */
export function parseSshDestination(text: string, port?: number): SshDestination {
  let destination = text.trim().replace(/^ssh\s+/u, '')
  let parsedPort = port
  const withPort = /^(.*[^:]):(\d{1,5})$/u.exec(destination)
  if (withPort !== null && !destination.startsWith('[') && withPort[1]!.split(':').length === 1) {
    destination = withPort[1]!
    parsedPort ??= Number(withPort[2])
  }
  if (destination.length > 255 || !DESTINATION.test(destination)) {
    throw new Error(localized({ zh: 'SSH 主机格式无效，请填写 user@host 或 ~/.ssh/config 中的主机别名。',
      en: 'Enter the SSH host as user@host or as a host alias from ~/.ssh/config.' }))
  }
  if (parsedPort !== undefined && (!Number.isInteger(parsedPort) || parsedPort < 1 || parsedPort > 65535)) {
    throw new Error(localized({ zh: 'SSH 端口必须是 1 到 65535 之间的整数。', en: 'The SSH port must be a whole number from 1 to 65535.' }))
  }
  return parsedPort === undefined || parsedPort === 22 ? { destination } : { destination, port: parsedPort }
}

/** @param target - destination. @returns `destination` or `destination:port`, the target's name. */
export function sshName(target: SshDestination): string {
  return target.port === undefined ? target.destination : `${target.destination}:${target.port}`
}

/**
 * Concrete host aliases of an OpenSSH config file; patterns with wildcards or negation are not hosts.
 * @param text - config file contents.
 * @returns aliases in file order.
 */
export function sshConfigHosts(text: string): string[] {
  const hosts: string[] = []
  for (const line of text.split(/\r?\n/u)) {
    const match = /^\s*Host\s+(.+?)\s*$/iu.exec(line)
    if (match === null) continue
    for (const name of match[1]!.split(/\s+/u)) {
      if (/[*?!]/u.test(name) || !DESTINATION.test(name) || hosts.includes(name)) continue
      hosts.push(name)
    }
  }
  return hosts
}

/** @returns the Windows OpenSSH client, or `ssh` from PATH elsewhere. */
export function sshExecutable(): string {
  if (process.platform !== 'win32') return 'ssh'
  const system = join(process.env.SystemRoot ?? 'C:\\Windows', 'System32', 'OpenSSH', 'ssh.exe')
  if (existsSync(system)) return system
  throw new Error(localized({ zh: '未找到 Windows 自带的 OpenSSH 客户端。请在“设置 → 系统 → 可选功能”中添加“OpenSSH 客户端”。',
    en: 'The Windows OpenSSH client was not found. Add "OpenSSH Client" under Settings → System → Optional features.' }))
}

/**
 * Arguments of one `ssh` run. Keep-alives notice a dropped connection within a minute.
 * @param target - destination.
 * @param remote - remote command words, already safe for the login shell.
 * @param forward - loopback port forwarded to the same port on the server.
 * @returns arguments for `sshExecutable()`.
 */
export function sshArguments(target: SshDestination, remote: readonly string[], forward?: number): string[] {
  return ['-T', '-o', 'ConnectTimeout=20', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=4',
    // Development and tests point at their own config; the application uses the user's.
    ...(process.env.RAINY_SSH_CONFIG ? ['-F', process.env.RAINY_SSH_CONFIG] : []),
    ...(target.port === undefined ? [] : ['-p', String(target.port)]),
    ...(forward === undefined ? [] : ['-L', `127.0.0.1:${forward}:127.0.0.1:${forward}`, '-o', 'ExitOnForwardFailure=yes']),
    '--', target.destination, ...remote]
}

/**
 * The login shell of the server (sh, bash, zsh, fish or csh) reads the command, so everything it sees is base64
 * except a fixed loader quoted with single quotes and free of `\`, `$` and `!`.
 */
const LOADER = '\'import base64,sys;a=[base64.b64decode(x).decode() for x in sys.argv[1:]];sys.argv=["rainy"]+a[1:];exec(a[0])\''

/**
 * Remote command that runs a Python script with arguments on the server.
 * @param script - Python source.
 * @param args - its arguments, received unchanged in `sys.argv[1:]`.
 * @returns command words for `sshArguments`.
 */
export function remotePython(script: string, args: readonly string[] = []): string[] {
  return ['python3', '-c', LOADER, ...[script, ...args].map(value => Buffer.from(value, 'utf8').toString('base64'))]
}

/** Starts the Host on the server with the carrier's environment, from the home directory. */
export const HOST_LAUNCHER = `import json,os,sys
p=json.loads(sys.argv[1])
os.chdir(os.path.expanduser("~"))
env=dict(os.environ)
env.update(p["env"])
os.execve(p["node"],[p["node"],p["host"]],env)
`

/** Options of one `ssh` run. */
export interface SshRunOptions {
  /** Askpass variables of this run. */
  environment?: Readonly<Record<string, string>>
  /** Sent to the remote command's standard input; closed immediately when absent. */
  input?: Readable
  /** Each standard error line, for progress and diagnostics. */
  onErrorLine?: (line: string) => void
  timeoutMs?: number
}

/** @param environment - askpass variables. @returns a child `ssh` process with piped standard streams. */
export function spawnSsh(args: readonly string[], environment: Readonly<Record<string, string>> = {}): ChildProcessWithoutNullStreams {
  return spawn(sshExecutable(), args, { windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'], env: { ...process.env, ...environment } })
}

/**
 * Run `ssh` to completion.
 * @param target - destination.
 * @param remote - remote command words.
 * @param options - askpass, input, progress and deadline.
 * @returns exit code, standard output and the last lines of standard error.
 */
export function runSsh(target: SshDestination, remote: readonly string[], options: SshRunOptions = {}): Promise<{ code: number | null; stdout: string; stderr: string }> {
  return new Promise((resolve, reject) => {
    let child: ChildProcessWithoutNullStreams
    try { child = spawnSsh(sshArguments(target, remote), options.environment) }
    catch (error) { reject(error instanceof Error ? error : new Error(String(error))); return }
    const stdout: Buffer[] = []
    const errors: string[] = []
    child.stdout.on('data', (chunk: Buffer) => { stdout.push(chunk) })
    createInterface({ input: child.stderr, crlfDelay: Infinity }).on('line', (line) => {
      if (!line.startsWith('RAINY_PROGRESS ')) { errors.push(line); if (errors.length > 40) errors.shift() }
      options.onErrorLine?.(line)
    })
    const timer = options.timeoutMs === undefined ? undefined : setTimeout(() => { child.kill() }, options.timeoutMs)
    if (options.input === undefined) child.stdin.end()
    else {
      options.input.on('error', (error) => { child.kill(); reject(error) })
      child.stdin.on('error', () => { /* The exit code reports a connection that closed early. */ })
      options.input.pipe(child.stdin)
    }
    child.once('error', (error) => { if (timer) clearTimeout(timer); reject(error) })
    child.once('close', (code) => {
      if (timer) clearTimeout(timer)
      resolve({ code, stdout: Buffer.concat(stdout).toString('utf8'), stderr: errors.join('\n') })
    })
  })
}

/**
 * Explain why `ssh` failed, from its exit code and standard error.
 * @param target - destination.
 * @param code - exit code.
 * @param stderr - last standard error lines.
 * @returns a localized message that ends with ssh's own words.
 */
export function sshFailure(target: SshDestination, code: number | null, stderr: string): Error {
  const name = sshName(target)
  const said = stderr.split('\n').map(line => line.trim()).filter(line => line !== '' && !line.startsWith('Warning: Permanently added')).slice(-3).join('\n')
  const reason = /REMOTE HOST IDENTIFICATION HAS CHANGED/u.test(stderr)
    ? localized({ zh: `${name} 的主机密钥与上次记录的不一致，可能有人冒充该服务器。确认服务器确实重装过后，再从 known_hosts 中删除旧记录。`,
      en: `The host key of ${name} differs from the one recorded earlier; someone may be impersonating the server. Remove the old entry from known_hosts only after confirming the server was reinstalled.` })
    : /Host key verification failed/u.test(stderr)
      ? localized({ zh: `没有信任 ${name} 的主机密钥，已取消连接。`, en: `The host key of ${name} was not trusted, so the connection was cancelled.` })
      : /Permission denied/u.test(stderr)
        ? localized({ zh: `${name} 拒绝了登录。请检查用户名、密码或密钥；也可以先在终端里用 ssh 登录一次确认。`,
          en: `${name} refused the login. Check the user name, password or key, or try logging in once with ssh in a terminal.` })
        : /Could not resolve hostname/u.test(stderr)
          ? localized({ zh: `找不到主机 ${name}。`, en: `The host ${name} cannot be found.` })
          : /Connection refused|timed out|No route to host|Network is unreachable|Connection reset|Connection closed/u.test(stderr)
            ? localized({ zh: `无法连接 ${name}，请检查网络、地址和端口。`, en: `Rainy cannot reach ${name}. Check the network, address and port.` })
            : code === 127 || /python3: (?:command )?not found/u.test(stderr)
              ? localized({ zh: `${name} 上没有 python3。Rainy 需要它来安装和启动远程后端，请先在服务器上安装 python3。`,
                en: `${name} has no python3. Rainy needs it to install and start its backend there; install python3 on the server first.` })
              : localized({ zh: `与 ${name} 的 SSH 连接失败（退出代码 ${code ?? '无'}）。`, en: `The SSH connection to ${name} failed (exit code ${code ?? 'none'}).` })
  return new Error(said === '' ? reason : `${reason}\n${said}`)
}
