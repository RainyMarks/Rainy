/** Offline imports reject damaged archives and altered generations before changing active selections. */
import { afterEach, describe, expect, it } from 'vitest'
import { mkdtemp, mkdir, readFile, writeFile, rm, stat } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { isAbsolute, join, relative } from 'node:path'
import { c as archiveTar } from 'tar'
import { componentDigest, environmentComponentSchema, installWindowsComponent } from '../src/environment-components.ts'

const directories: string[] = []
afterEach(async () => {
  for (const directory of directories.splice(0)) {
    const child = relative(tmpdir(), directory)
    if (!child || child.startsWith('..') || isAbsolute(child)) throw new Error('Component fixture cleanup escaped its temporary directory.')
    await rm(directory, { recursive: true, force: true })
  }
})
async function fixture(unlisted = false) {
  const directory = await mkdtemp(join(tmpdir(), 'rainy-component-test-'))
  directories.push(directory)
  const payload = join(directory, 'payload')
  const root = join(directory, 'installed')
  await mkdir(payload)
  await writeFile(join(payload, 'python.exe'), 'owned runtime fixture\n')
  const entry = { path: 'python.exe', bytes: (await stat(join(payload, 'python.exe'))).size, sha256: await componentDigest(join(payload, 'python.exe')) }
  await writeFile(join(payload, 'component.json'), JSON.stringify({ version: 1, id: 'windows-basic', platform: 'windows', architecture: 'x64',
    files: [entry], unpackedBytes: entry.bytes, python: '3.12.14' }))
  const files = ['component.json', 'python.exe']
  if (unlisted) { await writeFile(join(payload, 'unlisted.txt'), 'unlisted'); files.push('unlisted.txt') }
  const archive = join(directory, 'windows-basic.tar.gz')
  await archiveTar({ file: archive, cwd: payload, gzip: true }, files)
  const component = environmentComponentSchema.parse({ version: 1, id: 'windows-basic', platform: 'windows', architecture: 'x64', file: 'windows-basic.tar.gz',
    bytes: (await stat(archive)).size, sha256: await componentDigest(archive), unpackedBytes: entry.bytes, manifestSha256: await componentDigest(join(payload, 'component.json')) })
  return { directory, root, archive, options: { mediaDirectory: directory, root, component } }
}

describe('verified offline components', () => {
  it('imports and reuses one complete generation without changing its selected identity', async () => {
    const value = await fixture()
    const installed = await installWindowsComponent(value.options)
    expect(await readFile(join(installed, 'python.exe'), 'utf8')).toBe('owned runtime fixture\n')
    expect(await installWindowsComponent(value.options)).toBe(installed)
    expect(JSON.parse(await readFile(join(value.root, 'active.json'), 'utf8'))).toEqual({ 'windows-basic': `windows-basic/${value.options.component.sha256.slice(0, 32)}` })
  })
  it('rejects a damaged archive and preserves an existing active selection', async () => {
    const value = await fixture()
    await installWindowsComponent(value.options)
    const before = await readFile(join(value.root, 'active.json'), 'utf8')
    await writeFile(value.archive, 'damaged')
    await expect(installWindowsComponent(value.options)).rejects.toThrow('corrupt')
    expect(await readFile(join(value.root, 'active.json'), 'utf8')).toBe(before)
  })
  it('rechecks installed files before reusing an existing generation', async () => {
    const value = await fixture()
    const installed = await installWindowsComponent(value.options)
    await writeFile(join(installed, 'python.exe'), 'changed')
    await expect(installWindowsComponent(value.options)).rejects.toThrow('verification failed')
  })
  it('refuses an archive whose signed inventory omits a file', async () => {
    const value = await fixture(true)
    await expect(installWindowsComponent(value.options)).rejects.toThrow('unlisted')
    await expect(readFile(join(value.root, 'active.json'))).rejects.toHaveProperty('code', 'ENOENT')
  })
  it('rejects a destination that would exceed native Windows library loading limits', async () => {
    const value = await fixture()
    const root = join(value.root, 'a'.repeat(140))
    await expect(installWindowsComponent({ ...value.options, root })).rejects.toThrow('too long')
    await expect(readFile(join(root, 'active.json'))).rejects.toHaveProperty('code', 'ENOENT')
  })
})
