/** Permission modes, approvals, remembered rules, undo of file changes, and the grep and glob tools. */
import { mkdir, readFile, rm, symlink, writeFile } from 'node:fs/promises'
import { join, resolve } from 'node:path'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import type { PendingApproval, SessionSnapshot, TranscriptEntry } from '../../src/shared/rpc.ts'
import { canonicalPath, commandAllowed, commandRule, decide, READ_ONLY_REASON, type PermissionInput } from '../../src/host/agent/permissions.ts'
import { Observations, type ToolContext } from '../../src/host/agent/tools/common.ts'
import { globTool, grepTool } from '../../src/host/agent/tools/search.ts'
import { FakeOpenAI, harness, idle, tempHost, type Harness, type ScriptedReply } from './agent-fixtures.ts'

const shell = process.platform === 'win32' ? 'pwsh' : 'bash'
type ToolResultEntry = Extract<TranscriptEntry, { kind: 'toolResult' }>

function results(snapshot: SessionSnapshot): ToolResultEntry[] {
  return snapshot.entries.filter((entry): entry is ToolResultEntry => entry.kind === 'toolResult')
}

async function waitFor<T>(probe: () => Promise<T | undefined>): Promise<T> {
  for (let attempt = 0; attempt < 200; attempt++) {
    const value = await probe()
    if (value !== undefined) return value
    await new Promise(done => setTimeout(done, 25))
  }
  throw new Error('condition not reached')
}

async function pending(h: Harness, sessionId: string): Promise<PendingApproval> {
  return waitFor(async () => (await h.call('sessions.get', { sessionId })).approvals[0])
}

describe('permission rules', () => {
  it('offers a rule only for simple commands and keeps subcommands of package tools', () => {
    expect(commandRule('python3 solve.py')).toBe('python3')
    expect(commandRule('git status --short')).toBe('git status')
    expect(commandRule('pip install pwntools')).toBe('pip install')
    expect(commandRule('ls -la')).toBe('ls')
    for (const compound of ['ls; rm -rf /', 'cat a | sh', 'echo $(id)', 'echo `id`', 'a && b', 'x > out', 'sudo ls', 'FOO=1 ls', 'python3 -c "print(1)"', 'bash -c ls', 'pwsh -Command ls']) {
      expect(commandRule(compound), compound).toBeUndefined()
    }
  })

  it('matches remembered prefixes word by word, never for compound or inline-code commands', () => {
    expect(commandAllowed('python3 solve.py --remote', ['python3'], false)).toBe(true)
    expect(commandAllowed('python3 -c "import os"', ['python3'], false)).toBe(false)
    // Flag clusters and attached forms run inline code too.
    for (const inline of ['bash -lc ls', 'node -pe 1', "python3 -Bc 'import os'", 'node --eval=1', 'perl -we 1']) expect(commandAllowed(inline, ['bash', 'node', 'python3', 'perl'], false)).toBe(false)
    expect(commandAllowed('node --test', ['node'], false)).toBe(true)
    expect(commandAllowed('python3 solve.py; rm -rf ~', ['python3'], false)).toBe(false)
    expect(commandAllowed('git push', ['git status'], false)).toBe(false)
    expect(commandAllowed('/tmp/x/solve', ['./solve'], false)).toBe(false)
    expect(commandAllowed('Get-ChildItem -Force', ['get-childitem'], true)).toBe(true)
    expect(commandAllowed('Get-ChildItem -Force', ['get-childitem'], false)).toBe(false)
  })

  it('decides by mode: reads always run, read-only refuses changes, ask asks outside the project and for commands', async () => {
    const { dir, cleanup } = await tempHost()
    try {
      const project = await canonicalPath(join(dir, 'project'))
      await mkdir(project, { recursive: true })
      await mkdir(join(dir, 'outside'), { recursive: true })
      await symlink(join(dir, 'outside'), join(project, 'escape'), process.platform === 'win32' ? 'junction' : 'dir')
      const base: PermissionInput = {
        mode: 'ask', toolName: 'write', args: {}, cwd: project, roots: () => Promise.resolve([project]), rules: { commands: [], tools: [] }, readOnlyTools: new Set(['mcp__ida__decompile']),
        canRemember: true, windows: process.platform === 'win32',
      }
      expect(await decide({ ...base, mode: 'read-only', toolName: 'grep' })).toEqual({ kind: 'allow' })
      expect(await decide({ ...base, mode: 'read-only', args: { file_path: 'a.txt' } })).toEqual({ kind: 'deny', reason: READ_ONLY_REASON })
      expect(await decide({ ...base, mode: 'read-only', toolName: 'mcp__ida__decompile' })).toEqual({ kind: 'allow' })
      expect(await decide({ ...base, args: { file_path: 'notes/a.txt' } })).toEqual({ kind: 'allow' })
      const outside = await decide({ ...base, args: { file_path: '../outside/a.txt' } })
      expect(outside).toMatchObject({ kind: 'ask', approval: { reason: 'outside-write' } })
      // A link inside the project that points outside it does not make the write a project write.
      expect(await decide({ ...base, args: { file_path: 'escape/a.txt' } })).toMatchObject({ kind: 'ask', approval: { reason: 'outside-write', path: resolve(await canonicalPath(join(dir, 'outside')), 'a.txt') } })
      expect(await decide({ ...base, toolName: shell, args: { command: 'python3 solve.py' } })).toEqual({ kind: 'ask', approval: { reason: 'command', rule: 'python3' } })
      expect(await decide({ ...base, toolName: shell, args: { command: 'python3 solve.py' }, rules: { commands: ['python3'], tools: [] } })).toEqual({ kind: 'allow' })
      expect(await decide({ ...base, toolName: shell, args: { command: 'ls' }, canRemember: false })).toEqual({ kind: 'ask', approval: { reason: 'command' } })
      expect(await decide({ ...base, toolName: 'mcp__web__fetch' })).toEqual({ kind: 'ask', approval: { reason: 'mcp', rule: 'mcp__web__fetch' } })
      expect(await decide({ ...base, mode: 'auto', toolName: shell, args: { command: 'rm -rf build' } })).toEqual({ kind: 'allow' })
    } finally {
      await cleanup()
    }
  })
})

describe('approvals', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI | undefined
  beforeEach(() => { cleanups.length = 0 })
  afterEach(async () => {
    for (const cleanup of cleanups.reverse()) await cleanup()
    await server?.stop()
    server = undefined
  })

  async function start(replies: ScriptedReply[], mode: 'ask' | 'read-only' = 'ask'): Promise<{ h: Harness; sessionId: string }> {
    server = new FakeOpenAI(replies)
    await server.start()
    const h = await harness(server, cleanups, mode)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    return { h, sessionId: chat.id }
  }
  const echo = (text: string): ScriptedReply => ({ toolCalls: [{ name: shell, arguments: { command: `echo ${text}`, description: 'Print text' } }] })

  it('waits for approval, runs the command once allowed, and shows the wait in the history row', async () => {
    const { h, sessionId } = await start([echo('approved-run'), { text: 'done' }])
    await h.call('chat.send', { sessionId, text: 'run it' })
    const approval = await pending(h, sessionId)
    expect(approval).toMatchObject({ toolName: shell, reason: 'command', rule: 'echo' })
    expect((await h.call('sessions.list', {})).find(chat => chat.id === sessionId)?.awaitingApproval).toBe(true)
    await h.call('chat.approve', { sessionId, toolCallId: approval.toolCallId, decision: 'allow' })
    await idle(h, sessionId)
    const snapshot = await h.call('sessions.get', { sessionId })
    expect(results(snapshot)[0]!.isError).toBe(false)
    expect(JSON.stringify(results(snapshot)[0]!.content)).toContain('approved-run')
    expect(snapshot.approvals).toEqual([])
  })

  it('stops the run after a plain refusal and tells the model', async () => {
    const { h, sessionId } = await start([echo('never'), { text: 'should not be requested' }])
    await h.call('chat.send', { sessionId, text: 'run it' })
    const approval = await pending(h, sessionId)
    await h.call('chat.approve', { sessionId, toolCallId: approval.toolCallId, decision: 'deny' })
    await idle(h, sessionId)
    const snapshot = await h.call('sessions.get', { sessionId })
    const result = results(snapshot)[0]!
    expect(result.isError).toBe(true)
    expect(result.details).toEqual({ refused: 'user' })
    expect(JSON.stringify(result.content)).toContain('The user denied this tool call')
    expect(server!.requests).toHaveLength(1)
  })

  it('passes a refusal with instructions to the model and continues', async () => {
    const { h, sessionId } = await start([echo('never'), { text: 'Using python instead.' }])
    await h.call('chat.send', { sessionId, text: 'run it' })
    const approval = await pending(h, sessionId)
    await h.call('chat.approve', { sessionId, toolCallId: approval.toolCallId, decision: 'deny', reason: 'use python instead' })
    await idle(h, sessionId)
    expect(server!.requests).toHaveLength(2)
    expect(JSON.stringify(server!.requests[1])).toContain('The user denied this tool call and said: use python instead')
  })

  it('remembers "always allow" for the project and runs the next matching command without asking', async () => {
    const { h, sessionId } = await start([echo('first'), echo('second'), { text: 'done' }])
    await h.call('chat.send', { sessionId, text: 'run twice' })
    const approval = await pending(h, sessionId)
    await h.call('chat.approve', { sessionId, toolCallId: approval.toolCallId, decision: 'always' })
    await idle(h, sessionId)
    const snapshot = await h.call('sessions.get', { sessionId })
    expect(results(snapshot).map(result => result.isError)).toEqual([false, false])
    expect((await h.call('security.status', { workspaceId: h.workspaceId })).rules.commands).toEqual(['echo'])
    const removed = await h.call('security.removeRule', { workspaceId: h.workspaceId, kind: 'command', rule: 'echo' })
    expect(removed.rules.commands).toEqual([])
  })

  it('treats a new message as an answer: the waiting call is refused and the message reaches the model', async () => {
    const { h, sessionId } = await start([echo('never'), { text: 'ok, reading instead' }])
    await h.call('chat.send', { sessionId, text: 'run it' })
    await pending(h, sessionId)
    await h.call('chat.send', { sessionId, text: 'do not run commands, just read' })
    await idle(h, sessionId)
    expect(server!.requests).toHaveLength(2)
    const second = JSON.stringify(server!.requests[1])
    expect(second).toContain('sent a new message instead')
    expect(second).toContain('do not run commands, just read')
  })

  it('refuses changes in read-only mode and lets reads run', async () => {
    const { h, sessionId } = await start([
      { toolCalls: [{ name: 'write', arguments: { file_path: 'x.txt', content: 'x' } }] },
      { toolCalls: [{ name: 'glob', arguments: { pattern: '*.txt' } }] },
      { text: 'done' },
    ], 'read-only')
    await h.call('chat.send', { sessionId, text: 'write x' })
    await idle(h, sessionId)
    const snapshot = await h.call('sessions.get', { sessionId })
    expect(snapshot.permission).toBe('read-only')
    expect(results(snapshot)[0]).toMatchObject({ isError: true, details: { refused: 'mode' } })
    expect(results(snapshot)[1]!.isError).toBe(false)
    await expect(readFile(join(h.project, 'x.txt'), 'utf8')).rejects.toThrow()
  })

  it('switching the chat to full auto lets a waiting call run', async () => {
    const { h, sessionId } = await start([echo('after-switch'), { text: 'done' }])
    await h.call('chat.send', { sessionId, text: 'run it' })
    await pending(h, sessionId)
    expect(await h.call('chat.setPermission', { sessionId, mode: 'auto' })).toBe('auto')
    await idle(h, sessionId)
    const snapshot = await h.call('sessions.get', { sessionId })
    expect(snapshot.permission).toBe('auto')
    expect(JSON.stringify(results(snapshot)[0]!.content)).toContain('after-switch')
  })

  it('keeps an open chat in its mode when the default for new chats changes', async () => {
    const { h, sessionId } = await start([echo('still-asks'), { text: 'done' }])
    await h.call('security.setDefault', { mode: 'auto' })
    expect(h.events).toContainEqual({ event: 'security.changed', data: { defaultMode: 'auto' } })
    expect((await h.call('sessions.get', { sessionId })).permission).toBe('ask')
    await h.call('chat.send', { sessionId, text: 'run it' })
    expect((await pending(h, sessionId)).toolName).toBe(shell)
    const next = await h.call('sessions.create', { workspaceId: h.workspaceId })
    expect((await h.call('sessions.get', { sessionId: next.id })).permission).toBe('auto')
  })
})

describe('undo', () => {
  const cleanups: (() => Promise<void>)[] = []
  let server: FakeOpenAI | undefined
  beforeEach(() => { cleanups.length = 0 })
  afterEach(async () => {
    for (const cleanup of cleanups.reverse()) await cleanup()
    await server?.stop()
    server = undefined
  })

  it('restores edited files and removes created ones, newest first, and tells the model', async () => {
    server = new FakeOpenAI([
      { toolCalls: [{ name: 'read', arguments: { file_path: 'notes.txt' } }] },
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'notes.txt', old_string: 'alpha', new_string: 'beta' } }] },
      { toolCalls: [{ name: 'edit', arguments: { file_path: 'notes.txt', old_string: 'beta', new_string: 'gamma' } }] },
      { toolCalls: [{ name: 'write', arguments: { file_path: 'new/solve.py', content: 'print(1)\n' } }] },
      { text: 'done' },
    ])
    await server.start()
    const h = await harness(server, cleanups)
    await writeFile(join(h.project, 'notes.txt'), 'alpha\r\n')
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'change things' })
    await idle(h, chat.id)
    const changes = results(await h.call('sessions.get', { sessionId: chat.id })).filter(result => result.toolName !== 'read')
    expect(changes.map(change => (change.details as { undoable?: boolean }).undoable)).toEqual([true, true, true])
    expect(await readFile(join(h.project, 'notes.txt'), 'utf8')).toBe('gamma\r\n')
    const outcome = await h.call('chat.revert', { sessionId: chat.id, toolCallIds: changes.map(change => change.toolCallId) })
    expect(outcome.failed).toEqual([])
    expect(outcome.reverted.map(item => item.toolCallId)).toEqual(changes.map(change => change.toolCallId).reverse())
    expect(await readFile(join(h.project, 'notes.txt'), 'utf8')).toBe('alpha\r\n')
    await expect(readFile(join(h.project, 'new/solve.py'), 'utf8')).rejects.toThrow()
    const notice = (await h.call('sessions.get', { sessionId: chat.id })).entries.at(-1)
    expect(notice).toMatchObject({ kind: 'context', label: 'notice', reverted: outcome.reverted.map(item => item.toolCallId) })
    expect(JSON.stringify(notice)).toContain('notes.txt')
    // Undone changes are not undone twice.
    expect(await h.call('chat.revert', { sessionId: chat.id, toolCallIds: [changes[0]!.toolCallId] })).toEqual({ reverted: [], failed: [] })
  })

  it('leaves a file alone when it changed after the agent wrote it', async () => {
    server = new FakeOpenAI([{ toolCalls: [{ name: 'write', arguments: { file_path: 'flag.txt', content: 'agent\n' } }] }, { text: 'done' }])
    await server.start()
    const h = await harness(server, cleanups)
    const chat = await h.call('sessions.create', { workspaceId: h.workspaceId })
    await h.call('chat.send', { sessionId: chat.id, text: 'write' })
    await idle(h, chat.id)
    await writeFile(join(h.project, 'flag.txt'), 'user edit\n')
    const write = results(await h.call('sessions.get', { sessionId: chat.id }))[0]!
    const outcome = await h.call('chat.revert', { sessionId: chat.id, toolCallIds: [write.toolCallId] })
    expect(outcome.reverted).toEqual([])
    expect(outcome.failed).toMatchObject([{ toolCallId: write.toolCallId, reason: 'changed' }])
    expect(await readFile(join(h.project, 'flag.txt'), 'utf8')).toBe('user edit\n')
    await h.call('sessions.delete', { sessionId: chat.id })
    await expect(readFile(join(h.home, 'snapshots', chat.id), 'utf8')).rejects.toThrow()
  })
})

describe('grep and glob', () => {
  let dir: string
  let cleanup: () => Promise<void>
  let context: ToolContext
  beforeEach(async () => {
    const host = await tempHost()
    dir = join(host.dir, 'work')
    cleanup = host.cleanup
    await mkdir(join(dir, 'sub'), { recursive: true })
    await writeFile(join(dir, 'a-1-b.py'), 'alpha\nbeta flag{x}\ngamma\n')
    await writeFile(join(dir, 'sub', '.hidden'), 'flag{y}\n')
    await writeFile(join(dir, 'sub', 'c.txt'), 'nothing here\n')
    context = {
      cwd: dir, sessionId: 's', spillDir: join(host.dir, 'spill'), toolTokens: () => 4000, observations: new Observations(),
      runtimeEnvironment: () => ({ environment: {}, executables: {} }), ripgrep: (await import('@vscode/ripgrep')).rgPath,
    }
  })
  afterEach(async () => { await cleanup() })

  const text = (result: { content: { type: string; text?: string }[] }): string => result.content.map(block => block.text ?? '').join('')

  it('lists matching files, hidden ones included, relative to the workspace', async () => {
    const result = await grepTool(context).execute('1', { pattern: 'flag\\{' })
    expect(text(result).split('\n').sort()).toEqual(['a-1-b.py', 'sub/.hidden'])
    expect(result.details).toMatchObject({ truncated: false, paths: expect.arrayContaining(['a-1-b.py', 'sub/.hidden']) })
  })

  it('shows matching lines with numbers and context, parsing names that contain dashes', async () => {
    const result = await grepTool(context).execute('2', { pattern: 'beta', output_mode: 'content', context: 1 })
    expect(text(result)).toBe('a-1-b.py-1-alpha\na-1-b.py:2:beta flag{x}\na-1-b.py-3-gamma')
    expect(result.details.files).toEqual([{ path: 'a-1-b.py', matches: [{ lineNumber: 1, line: 'alpha' }, { lineNumber: 2, line: 'beta flag{x}' }, { lineNumber: 3, line: 'gamma' }] }])
    expect(text(await grepTool(context).execute('3', { pattern: 'zzz' }))).toBe('No matches found.')
  })

  it('finds files by glob, newest first, with a limit', async () => {
    const result = await globTool(context).execute('4', { pattern: '*.txt' })
    expect(text(result)).toBe('sub/c.txt')
    const all = await globTool(context).execute('5', { pattern: '*', limit: 1 })
    expect(all.details).toMatchObject({ truncated: true })
    expect(text(all)).toContain('[Showing 1 of 3 files')
    await rm(join(dir, 'sub'), { recursive: true })
  })
})
