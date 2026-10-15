/** Text search across a project's roots and replacement on disk and in text. */
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { afterEach, beforeAll, describe, expect, it } from 'vitest'
import { brandString } from '../../src/shared/brand.ts'
import type { IdeRootId, WorkspaceId } from '../../src/shared/ide-files-protocol.ts'
import { previewLine, replaceInText, type TextSearchOptions } from '../../src/shared/text-search.ts'
import type { Project } from '../../src/host/projects.ts'
import { globArguments, TextSearch } from '../../src/host/search/index.ts'
import { tempHost } from './agent-fixtures.ts'

const workspaceId = brandString<WorkspaceId>('w1')
const primary = brandString<IdeRootId>('primary')
const extra = brandString<IdeRootId>('extra')
const plain: TextSearchOptions = { pattern: '', regex: false, caseSensitive: false, wholeWord: false, include: '', exclude: '' }

describe('replacing in text', () => {
  it('replaces literal text as typed, line by line, keeping line endings', () => {
    expect(replaceInText('a.b a.b\r\nxa.by\n', { ...plain, pattern: 'a.b' }, '$1')).toEqual({ text: '$1 $1\r\nx$1y\n', count: 3 })
    expect(replaceInText('Key key KEY', { ...plain, pattern: 'key', caseSensitive: true }, 'k')).toEqual({ text: 'Key k KEY', count: 1 })
    expect(replaceInText('key keys monkey', { ...plain, pattern: 'key', wholeWord: true }, 'K')).toEqual({ text: 'K keys monkey', count: 1 })
    expect(replaceInText('nothing', { ...plain, pattern: '' }, 'x')).toEqual({ text: 'nothing', count: 0 })
  })

  it('expands groups and escapes in a regular-expression replacement, and anchors to each line', () => {
    const options = { ...plain, regex: true, caseSensitive: true }
    expect(replaceInText('flag{abc}\nflag{de}', { ...options, pattern: 'flag\\{(\\w+)\\}' }, '[$1]')).toEqual({ text: '[abc]\n[de]', count: 2 })
    expect(replaceInText('x=1\ny=2', { ...options, pattern: '^(\\w)=' }, '$1 := ')).toEqual({ text: 'x := 1\ny := 2', count: 2 })
    expect(replaceInText('a,b', { ...options, pattern: ',' }, '\\n')).toEqual({ text: 'a\nb', count: 1 })
    expect(replaceInText('v1 v2', { ...options, pattern: '(?<name>v)(\\d)' }, '$<name>-$2-$&')).toEqual({ text: 'v-1-v1 v-2-v2', count: 2 })
    // A pattern never matches across lines, as in the search.
    expect(replaceInText('ab\ncd', { ...options, pattern: 'b\\s+c' }, 'X')).toEqual({ text: 'ab\ncd', count: 0 })
    expect(() => replaceInText('x', { ...options, pattern: '(' }, 'y')).toThrow()
  })

  it('previews a long line around its match', () => {
    expect(previewLine('short', 2)).toEqual({ preview: 'short', previewColumn: 2 })
    const long = `${'a'.repeat(500)}MATCH${'b'.repeat(500)}`
    const { preview, previewColumn } = previewLine(long, 500)
    expect(preview.startsWith('…')).toBe(true)
    expect(preview.endsWith('…')).toBe(true)
    expect(preview.slice(previewColumn, previewColumn + 5)).toBe('MATCH')
  })

  it('turns typed globs into ripgrep globs, a plain name covering folders too', () => {
    const args = globArguments({ include: 'src, *.py', exclude: 'build/' })
    expect(args.slice(0, 8)).toEqual(['--glob', 'src', '--glob', 'src/**', '--glob', '**/src/**', '--glob', '*.py'])
    expect(args.slice(8, 10)).toEqual(['--glob', '!build/**'])
  })
})

describe('searching a project', () => {
  let ripgrep: string
  const cleanups: (() => Promise<void>)[] = []
  beforeAll(async () => { ripgrep = (await import('@vscode/ripgrep')).rgPath })
  afterEach(async () => { for (const cleanup of cleanups.splice(0)) await cleanup() })

  async function project(files: Record<string, string | Buffer>, extraFiles: Record<string, string> = {}): Promise<{ root: string; second: string; search: TextSearch }> {
    const { dir, cleanup } = await tempHost()
    cleanups.push(cleanup)
    const root = join(dir, 'project')
    const second = join(dir, 'shared')
    for (const [base, entries] of [[root, files], [second, extraFiles]] as const) {
      await mkdir(base, { recursive: true })
      for (const [path, text] of Object.entries(entries)) {
        await mkdir(dirname(join(base, path)), { recursive: true })
        await writeFile(join(base, path), text)
      }
    }
    const record: Project = { id: workspaceId, path: root, title: 'project', attached: [{ rootId: extra, path: second, title: 'shared' }], createdAt: 0, openedAt: 0 }
    return { root, second, search: new TextSearch({ projects: { get: id => id === workspaceId ? record : undefined }, ripgrep }) }
  }

  it('finds matches in every root with their lines and columns, skipping ignored folders', async () => {
    const { search } = await project({
      'solve.py': 'key = b"r41n"\nprint(KEY)\n',
      'src/util.py': '# 注释 key\n',
      '.gitignore': 'build/\n',
      'build/out.py': 'key\n',
      'node_modules/x/key.js': 'key\n',
    }, { 'notes.md': 'the key\n' })
    const result = await search.search({ ...plain, workspaceId, pattern: 'key' })
    expect(result.truncated).toBe(false)
    expect(result.files.map(file => `${file.rootId}:${file.path}`)).toEqual(['primary:solve.py', 'primary:src/util.py', 'extra:notes.md'])
    expect(result.matches).toBe(4)
    expect(result.files[0]!.matches.map(match => [match.line, match.column, match.length])).toEqual([[1, 0, 3], [2, 6, 3]])
    // Columns count UTF-16 code units, so they land on the right characters in the editor.
    expect(result.files[1]!.matches[0]).toMatchObject({ line: 1, column: 5, length: 3, preview: '# 注释 key', previewColumn: 5 })
  })

  it('honours case, whole words, regular expressions and globs, and reports a bad pattern', async () => {
    const { search } = await project({ 'a.py': 'Key keys key\n', 'b.txt': 'key\n', 'c/d.py': 'key9\n' })
    const count = async (change: Partial<TextSearchOptions>) => (await search.search({ ...plain, workspaceId, pattern: 'key', ...change })).matches
    expect(await count({ caseSensitive: true, include: '*.py' })).toBe(3)
    expect(await count({ wholeWord: true })).toBe(3)
    expect(await count({ regex: true, pattern: 'key\\d' })).toBe(1)
    expect(await count({ exclude: 'c, *.txt' })).toBe(3)
    expect(await count({ include: 'c' })).toBe(1)
    await expect(search.search({ ...plain, workspaceId, pattern: '(', regex: true })).rejects.toThrow(/invalid-pattern|正则|regular/u)
  })

  it('ends an older search of the same project when a newer one starts', async () => {
    const { search } = await project({ 'a.txt': 'key\n' })
    const first = search.search({ ...plain, workspaceId, pattern: 'key' }).then(result => result.matches, (error: unknown) => String(error))
    const second = search.search({ ...plain, workspaceId, pattern: 'key' })
    expect((await second).matches).toBe(1)
    expect(await first).toMatch(/abort/iu)
  })

  it('replaces on disk, keeping the BOM and line endings, and refuses files it cannot change safely', async () => {
    const { root, second, search } = await project({
      'a.py': '﻿key = 1\r\nkey2 = key\r\n',
      'latin.txt': Buffer.from([0x6b, 0x65, 0x79, 0xe9, 0x0a]),
    }, { 'b.md': 'no match here\n' })
    const result = await search.replace({
      ...plain, workspaceId, pattern: 'key', wholeWord: true, replacement: 'secret',
      files: [{ rootId: primary, path: 'a.py' }, { rootId: primary, path: 'latin.txt' }, { rootId: extra, path: 'b.md' }, { rootId: primary, path: '../escape.txt' }],
    })
    expect(result.files).toEqual([{ rootId: primary, path: 'a.py', replaced: 2 }, { rootId: extra, path: 'b.md', replaced: 0 }])
    expect(result.failed.map(item => item.path)).toEqual(['latin.txt', '../escape.txt'])
    expect(await readFile(join(root, 'a.py'), 'utf8')).toBe('﻿secret = 1\r\nkey2 = secret\r\n')
    expect(await readFile(join(second, 'b.md'), 'utf8')).toBe('no match here\n')
  })
})
