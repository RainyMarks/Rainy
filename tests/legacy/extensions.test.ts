import { describe, it, expect } from 'vitest'
import { parseExtensions, officialIdaServer, skillDescriptor } from '../src/extensions.ts'
describe('session extension choices', () => {
  it('exposes concise purpose and a read path without eagerly injecting the skill body', () => {
    const body = '---\nname: python-quality\ndescription: Run focused Python verification.\n---\n\n# Detailed instructions\n' + 'long detailed content '.repeat(5000)
    const value = skillDescriptor(body, 'project/python-quality', '/project/.rainy/skills/python-quality/SKILL.md', 512)
    expect(value).toContain('Run focused Python verification.')
    expect(value).toContain('Read "/project/.rainy/skills/python-quality/SKILL.md"')
    expect(value).not.toContain('long detailed content')
    expect(value.length).toBeLessThan(300)
  })
  it('keeps an empty session free of extension processes and schemas', () =>{  expect(parseExtensions({ skills: [], servers: [] })).toEqual({ skills: [], servers: [] }) })
  it('requires an explicit MCP tool allowlist', () =>{  expect(() => parseExtensions({ skills: [], servers: [{ serverName: 'echo', transport: 'stdio', command: 'node', tools: [] }] })).toThrow() })
  it('retains separate process arguments', () => {
    const server = parseExtensions({ skills: [], servers: [{ serverName: 'echo', transport: 'stdio', command: '/a b/node', args: ['/中文/test.mjs'], tools: ['echo'] }] }).servers[0]
    expect(server?.transport === 'stdio' && server.args).toEqual(['/中文/test.mjs'])
  })
  it('rejects duplicate service names', () => {
    const server = { serverName: 'echo', transport: 'streamable-http', url: 'http://localhost:1234/mcp', tools: ['echo'] }
    expect(() => parseExtensions({ skills: [], servers: [server, server] })).toThrow()
  })
  it('persists the optional official IDA selection and rejects conflicting servers', () => {
    expect(parseExtensions({ skills: [], servers: [], ida: true })).toEqual({ skills: [], servers: [], ida: true })
    expect(() => parseExtensions({ skills: [], servers: [], ida: 'true' })).toThrow('ida 必须')
    expect(() => parseExtensions({ skills: [], ida: true, servers: [{ serverName: 'ida', transport: 'stdio', command: 'custom', tools: ['list'] }] })).toThrow('不能同时')
  })
  it('ignores the retired CTF mode in existing session files', () => {
    expect(parseExtensions({ skills: [], servers: [], mode: 'ctf' })).toEqual({ skills: [], servers: [] })
    expect(() => parseExtensions({ skills: [], servers: [], mode: 'prompt-injection' })).toThrow('旧版 mode')
  })
  it('uses the official installed package through an explicit WSL executable path', () => {
    const command = '/mnt/c/Users/Test User/uvx.exe'
    const server = officialIdaServer(command)
    expect(server.transport === 'stdio' && server.command).toBe(command)
    expect(server.transport === 'stdio' && server.args).toContain('ida-mcp==20260924.0.3')
    expect(server.tools).toContain('list_databases')
    expect(() => officialIdaServer(undefined)).toThrow('未找到')
    expect(() => officialIdaServer('uvx.exe')).toThrow('未找到')
  })
})
