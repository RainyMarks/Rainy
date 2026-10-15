/** The Git panel's Host side: status, staging, discarding, committing and the AI commit message. */
import { execFileSync } from 'node:child_process'
import { mkdir, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { IdeRootId, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { cleanMessage, GitService, parseStatus } from '../../src/host/git/index.ts'
import type { Project } from '../../src/host/projects.ts'
import { tempHost } from './agent-fixtures.ts'

const workspaceId = brandString<WorkspaceId>('w1')
const primary = brandString<IdeRootId>('primary')

describe('reading git status', () => {
  it('lists changes inside the root with branch facts, renames and conflicts', () => {
    const output = [
      '# branch.oid 0123456789abcdef', '# branch.head main', '# branch.upstream origin/main', '# branch.ab +2 -1',
      '1 .M N... 100644 100644 100644 aaaa bbbb app/solve.py',
      '1 A. N... 000000 100644 100644 0000 cccc app/new file.py',
      '2 R. N... 100644 100644 100644 dddd dddd R100 app/b.py', 'app/a.py',
      'u UU N... 100644 100644 100644 100644 e f g app/conflict.py',
      '? app/notes.txt',
      '1 .M N... 100644 100644 100644 aaaa bbbb other/outside.py',
      '',
    ].join('\0')
    expect(parseStatus(output, 'app/')).toEqual({
      branch: 'main', head: '0123456', upstream: 'origin/main', ahead: 2, behind: 1, truncated: false,
      files: [
        { path: 'solve.py', unstaged: 'modified' },
        { path: 'new file.py', staged: 'added' },
        { path: 'b.py', from: 'a.py', staged: 'renamed' },
        { path: 'conflict.py', unstaged: 'conflicted' },
        { path: 'notes.txt', unstaged: 'untracked' },
      ],
    })
    expect(parseStatus('# branch.oid (initial)\0# branch.head (detached)\0', '')).toMatchObject({ branch: null, head: null, files: [] })
  })

  it('tidies a model reply into a commit message', () => {
    expect(cleanMessage('```\nAdd XOR helper\n\nMove the loop.  \n```')).toBe('Add XOR helper\n\nMove the loop.')
    expect(cleanMessage('"Fix empty key"')).toBe('Fix empty key')
  })
})

describe('working with a repository', () => {
  const cleanups: (() => Promise<void>)[] = []
  afterEach(async () => { for (const cleanup of cleanups.splice(0)) await cleanup() })

  async function repository(): Promise<{ root: string; git: GitService; prompts: string[]; run(...args: string[]): string }> {
    const { dir, cleanup } = await tempHost()
    cleanups.push(cleanup)
    const root = join(dir, 'project')
    await mkdir(join(root, 'sub'), { recursive: true })
    const run = (...args: string[]): string => execFileSync('git', ['-C', root, ...args], { encoding: 'utf8' })
    run('init', '-q', '-b', 'main')
    run('config', 'user.name', 'Tester')
    run('config', 'user.email', 'tester@example.com')
    run('config', 'core.autocrlf', 'false')
    await writeFile(join(root, 'solve.py'), 'print(1)\n')
    await writeFile(join(root, 'sub', 'helper.py'), 'x = 1\n')
    run('add', '-A')
    run('commit', '-q', '-m', 'Start')
    const record: Project = { id: workspaceId, path: root, title: 'project', attached: [], createdAt: 0, openedAt: 0 }
    const prompts: string[] = []
    const models = {
      resolve: () => ({ model: { provider: 'fake', id: 'fake-model' } }),
      complete: async (_resolved: unknown, prompt: string) => {
        prompts.push(prompt)
        return { stopReason: 'stop', content: [{ type: 'text', text: '```\nPrint two\n```' }] }
      },
    }
    const git = new GitService({ projects: { get: id => id === workspaceId ? record : undefined }, models: models as never, settings: { prefs: () => ({ locale: 'en' }) as never }, log: () => undefined })
    return { root, git, prompts, run }
  }

  it('stages, unstages, discards and commits, and lists what changed', async () => {
    const { root, git, run } = await repository()
    await writeFile(join(root, 'solve.py'), 'print(2)\n')
    await writeFile(join(root, 'notes.txt'), 'flag?\n')
    await rm(join(root, 'sub', 'helper.py'))
    let status = await git.status(workspaceId)
    expect(status.available).toBe(true)
    expect(status.repositories[0]).toMatchObject({ rootId: primary, branch: 'main', files: [
      { path: 'solve.py', unstaged: 'modified' }, { path: 'sub/helper.py', unstaged: 'deleted' }, { path: 'notes.txt', unstaged: 'untracked' },
    ] })

    await git.stage(workspaceId, primary, ['solve.py', 'sub/helper.py'])
    status = await git.status(workspaceId)
    expect(status.repositories[0]!.files.filter(file => file.staged !== undefined).map(file => `${file.path}:${file.staged}`).sort())
      .toEqual(['solve.py:modified', 'sub/helper.py:deleted'])
    await git.unstage(workspaceId, primary, ['sub/helper.py'])
    await git.discard(workspaceId, primary, ['sub/helper.py', 'notes.txt'])
    expect(await readFile(join(root, 'sub', 'helper.py'), 'utf8')).toBe('x = 1\n')
    await expect(stat(join(root, 'notes.txt'))).rejects.toThrow()

    const { commit } = await git.commit(workspaceId, primary, 'Print two\n\nBody line', false)
    expect(commit).toMatch(/^[0-9a-f]{7,}$/u)
    expect(run('log', '-1', '--format=%B').trim()).toBe('Print two\n\nBody line')
    expect((await git.status(workspaceId)).repositories[0]!.files).toEqual([])
    await expect(git.commit(workspaceId, primary, 'Again', false)).rejects.toMatchObject({ code: 'nothing-to-commit' })
    await expect(git.commit(workspaceId, primary, '  ', true)).rejects.toThrow()
  })

  it('commits everything in one step, and asks the model for a message about the changes', async () => {
    const { root, git, prompts, run } = await repository()
    await writeFile(join(root, 'solve.py'), 'print(2)\n')
    await writeFile(join(root, 'sub', 'new.py'), 'y = 2\n')
    expect(await git.message(workspaceId, primary)).toEqual({ message: 'Print two' })
    expect(prompts[0]).toContain('nothing is staged')
    expect(prompts[0]).toContain('-print(1)')
    expect(prompts[0]).toContain('sub/new.py')
    expect(prompts[0]).toContain('Start')
    await git.commit(workspaceId, primary, 'Print two', true)
    expect(run('show', '--name-only', '--format=', 'HEAD').trim().split('\n').sort()).toEqual(['solve.py', 'sub/new.py'])
  })

  it('reports a root outside any repository, and starts one there', async () => {
    const { dir, cleanup } = await tempHost()
    cleanups.push(cleanup)
    const root = join(dir, 'plain')
    await mkdir(root)
    const record: Project = { id: workspaceId, path: root, title: 'plain', attached: [], createdAt: 0, openedAt: 0 }
    const git = new GitService({ projects: { get: () => record }, models: {} as never, settings: { prefs: () => ({ locale: 'zh' }) as never }, log: () => undefined })
    // The temp directory may sit inside a repository on some machines; only a root of its own counts here.
    const before = await git.status(workspaceId)
    if (before.repositories.length === 0) expect(before.plain).toEqual([{ rootId: primary, title: 'plain' }])
    await git.init(workspaceId, primary)
    await writeFile(join(root, 'a.txt'), 'a\n')
    const after = await git.status(workspaceId)
    expect(after.repositories[0]).toMatchObject({ head: null, files: [{ path: 'a.txt', unstaged: 'untracked' }] })
    await git.stage(workspaceId, primary, ['a.txt'])
    await git.unstage(workspaceId, primary, ['a.txt'])
    expect((await git.status(workspaceId)).repositories[0]!.files).toEqual([{ path: 'a.txt', unstaged: 'untracked' }])
  })
})
