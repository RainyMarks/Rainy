/** SSH destinations, config aliases, ssh arguments, the remote Python loader and failure messages. */
import { execFileSync } from 'node:child_process'
import { describe, expect, it } from 'vitest'
import { HOST_LAUNCHER, parseSshDestination, remotePython, sshArguments, sshConfigHosts, sshFailure, sshName } from '../../src/main/ssh.ts'

describe('destinations', () => {
  it('accepts hosts, users, aliases, ports and bracketed IPv6', () => {
    expect(parseSshDestination('dev')).toEqual({ destination: 'dev' })
    expect(parseSshDestination('  ssh alice@build.example.com ')).toEqual({ destination: 'alice@build.example.com' })
    expect(parseSshDestination('alice@10.0.0.5:2222')).toEqual({ destination: 'alice@10.0.0.5', port: 2222 })
    expect(parseSshDestination('alice@10.0.0.5', 22)).toEqual({ destination: 'alice@10.0.0.5' })
    expect(parseSshDestination('root@[fe80::1]', 2200)).toEqual({ destination: 'root@[fe80::1]', port: 2200 })
    expect(sshName({ destination: 'dev', port: 2222 })).toBe('dev:2222')
  })

  it('rejects anything ssh could read as an option or a second word', () => {
    for (const text of ['-oProxyCommand=calc', 'a b', '-dev', 'alice@-host', '-x@host', 'host;rm', 'host$(id)', '', 'a'.repeat(300)]) {
      expect(() => parseSshDestination(text), text).toThrow()
    }
    expect(() => parseSshDestination('dev', 70000)).toThrow()
    expect(() => parseSshDestination('dev', 0)).toThrow()
  })

  it('lists concrete aliases of a config file and skips patterns', () => {
    expect(sshConfigHosts('Host dev build\n  HostName 10.0.0.1\nHost *.corp !bad gpu?\nhost   lab \nMatch all\nHost dev\n')).toEqual(['dev', 'build', 'lab'])
  })
})

describe('ssh arguments', () => {
  it('ends options with -- before the destination and forwards one loopback port to the same port', () => {
    const args = sshArguments({ destination: 'dev', port: 2222 }, ['python3'], 41000)
    expect(args.slice(args.indexOf('--'))).toEqual(['--', 'dev', 'python3'])
    expect(args).toEqual(expect.arrayContaining(['-T', '-p', '2222', '-L', '127.0.0.1:41000:127.0.0.1:41000', '-o', 'ExitOnForwardFailure=yes']))
    expect(sshArguments({ destination: 'dev' }, [])).not.toContain('-L')
  })

  it('passes the script and its arguments as base64 behind a loader free of shell metacharacters', () => {
    const words = remotePython('print(1)', ['{"a": "x y \' $HOME \\\\"}'])
    expect(words.slice(0, 2)).toEqual(['python3', '-c'])
    const loader = words[2]!
    expect(loader.startsWith('\'') && loader.endsWith('\'')).toBe(true)
    expect(loader.slice(1, -1)).not.toMatch(/['\\$!`]/u)
    for (const word of words.slice(3)) expect(word).toMatch(/^[A-Za-z0-9+/=]+$/u)
    expect(Buffer.from(words[4]!, 'base64').toString('utf8')).toBe('{"a": "x y \' $HOME \\\\"}')
  })

  it('runs the loader and the Host launcher with Python when one is available', (context) => {
    let python = ''
    for (const candidate of ['python3', 'python']) {
      try { execFileSync(candidate, ['--version'], { stdio: 'ignore' }); python = candidate; break } catch { /* try the next name */ }
    }
    if (python === '') { context.skip(); return }
    const words = remotePython('import sys\nprint(sys.argv[1:])\n', ['one two', '\'quoted\' $X'])
    const output = execFileSync(python, ['-c', words[2]!.slice(1, -1), ...words.slice(3)], { encoding: 'utf8' })
    expect(output.trim()).toBe(String.raw`['one two', "'quoted' $X"]`)
    expect(HOST_LAUNCHER).toContain('os.execve(p["node"],[p["node"],p["host"]],env)')
  })
})

describe('failures', () => {
  const target = { destination: 'dev' }
  it('names the cause and keeps what ssh said', () => {
    expect(sshFailure(target, 255, 'alice@dev: Permission denied (publickey,password).').message).toMatch(/拒绝了登录|refused the login/u)
    expect(sshFailure(target, 255, 'ssh: Could not resolve hostname dev: No such host is known.').message).toMatch(/找不到主机|cannot be found/u)
    expect(sshFailure(target, 255, 'ssh: connect to host dev port 22: Connection refused').message).toMatch(/无法连接|cannot reach/u)
    expect(sshFailure(target, 127, 'bash: line 1: python3: command not found').message).toMatch(/python3/u)
    expect(sshFailure(target, 255, '@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@').message).toMatch(/known_hosts/u)
    expect(sshFailure(target, 255, 'Host key verification failed.').message).toMatch(/没有信任|not trusted/u)
    expect(sshFailure(target, 1, 'Warning: Permanently added dev\nsomething else').message).toContain('something else')
  })
})
