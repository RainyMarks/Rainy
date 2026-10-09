/** Upgrade backups lose the program files that a saved inventory can restore, and keep everything else. */
import { createHash, randomUUID } from 'node:crypto'
import { chmod, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { afterEach, describe, expect, it } from 'vitest'
import { pruneToolPackBackups } from '../../src/main/toolpack-prune.ts'

const roots: string[] = []
afterEach(async () => { await Promise.all(roots.splice(0).map(root => rm(root, { recursive: true, force: true }))) })
const hash = (value: string): string => createHash('sha256').update(value).digest('hex')

async function put(root: string, path: string, value: string): Promise<string> {
  const target = join(root, ...path.split('/'))
  await mkdir(dirname(target), { recursive: true })
  await writeFile(target, value)
  return target
}

async function fixture(journalPhase = 'committed') {
  const root = await mkdtemp(join(tmpdir(), 'rainy-toolpack-prune-'))
  roots.push(root)
  const state = join(root, '.rainy-toolpack')
  const content = { 'tools/alpha/app.exe': 'alpha program', 'tools/alpha/config.ini': 'default=1', 'tools/manifest.json': '{"version":1,"tools":[]}' }
  const files = Object.entries(content).map(([path, value]) => ({ path, bytes: Buffer.byteLength(value), sha256: hash(value) }))
  const units = [{ path: 'tools/alpha', kind: 'directory', preserve: [] }, { path: 'tools/manifest.json', kind: 'file', preserve: [] }]
  const id = hash(JSON.stringify({ files, units }))
  await put(state, `manifests/${id}.json`, JSON.stringify({ version: 1, id, format: 'tar.gz', volumeSize: 1024,
    unpackedBytes: files.reduce((sum, file) => sum + file.bytes, 0), files, units,
    volumes: [{ file: `native-tools-${id.slice(0, 16)}.tar.gz.001`, bytes: 1, sha256: hash('volume') }] }))
  await put(state, 'journal.json', JSON.stringify({ phase: journalPhase }))
  const kept = randomUUID()
  const emptied = randomUUID()
  await put(state, `backups/${kept}/tools/alpha/app.exe`, content['tools/alpha/app.exe'])
  await put(state, `backups/${kept}/tools/alpha/config.ini`, 'default=2')
  await put(state, `backups/${kept}/tools/alpha/history/session.log`, 'personal history')
  await put(state, `backups/${emptied}/tools/alpha/app.exe`, content['tools/alpha/app.exe'])
  await chmod(await put(state, `backups/${emptied}/tools/manifest.json`, content['tools/manifest.json']), 0o444)
  return { root, state, kept, emptied }
}

describe('tool backup pruning', () => {
  it('deletes inventoried program files, keeps changed and personal files, and removes emptied backups', async () => {
    const test = await fixture()
    const result = await pruneToolPackBackups(test.root)
    expect(result).toEqual({ removedFiles: 3, removedBytes: 2 * 'alpha program'.length + '{"version":1,"tools":[]}'.length, keptFiles: 2 })
    expect(await readdir(join(test.state, 'backups'))).toEqual([test.kept])
    const backup = join(test.state, 'backups', test.kept, 'tools/alpha')
    expect((await readdir(backup)).sort()).toEqual(['config.ini', 'history'])
    expect(await readFile(join(backup, 'history/session.log'), 'utf8')).toBe('personal history')
  })

  it('limits pruning to the named transactions', async () => {
    const test = await fixture()
    await pruneToolPackBackups(test.root, { transactions: [test.emptied] })
    expect(await readdir(join(test.state, 'backups'))).toEqual([test.kept])
    expect(await readdir(join(test.state, 'backups', test.kept, 'tools/alpha'))).toContain('app.exe')
  })

  it.each(['switching', 'rolling-back'])('keeps every backup while a transaction is %s', async (phase) => {
    const test = await fixture(phase)
    expect(await pruneToolPackBackups(test.root)).toEqual({ removedFiles: 0, removedBytes: 0, keptFiles: 0 })
    expect((await readdir(join(test.state, 'backups'))).sort()).toEqual([test.kept, test.emptied].sort())
  })

  it('does nothing for a directory without backups', async () => {
    const root = await mkdtemp(join(tmpdir(), 'rainy-toolpack-prune-'))
    roots.push(root)
    expect(await pruneToolPackBackups(root)).toEqual({ removedFiles: 0, removedBytes: 0, keptFiles: 0 })
  })
})
