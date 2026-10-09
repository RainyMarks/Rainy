/** CLI packaging uses independent fixtures and verifies the emitted archive, metadata and publication. */
import { execFile } from 'node:child_process'
import { createHash, randomBytes } from 'node:crypto'
import { link, lstat, mkdir, mkdtemp, readFile, readdir, rename, rm, symlink, utimes, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { dirname, isAbsolute, join, relative, resolve } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { promisify } from 'node:util'
import { list as listTar } from 'tar'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { toolPackMetadataSchema, type ToolPackMetadataV2 } from '../src/toolpack-format.ts'

const execute = promisify(execFile)
const script = fileURLToPath(new URL('../scripts/package-native-tools.mjs', import.meta.url))
let fixture: string
let stage: string
let output: string

interface InventoryFile { path: string; bytes: number; sha256: string }
interface Inventory { version: number; bytes: number; files: InventoryFile[] }
interface ManifestTool { id: string; roots: string[]; preserve?: string[] }

beforeEach(async () => {
  fixture = await mkdtemp(join(tmpdir(), 'rainy-toolpack-package-'))
  stage = join(fixture, '离线工具 stage')
  output = join(fixture, '发行 output')
  await mkdir(stage)
})

afterEach(async () => {
  const child = relative(tmpdir(), fixture)
  if (child === '' || child.startsWith('..') || isAbsolute(child)) throw new Error('Test cleanup escaped the temporary directory.')
  await rm(fixture, { recursive: true, force: true })
})

async function writeStage(path: string, content: string | Buffer): Promise<InventoryFile> {
  const absolute = join(stage, ...path.split('/'))
  await mkdir(dirname(absolute), { recursive: true })
  await writeFile(absolute, content)
  const bytes = Buffer.isBuffer(content) ? content : Buffer.from(content)
  return { path, bytes: bytes.length, sha256: createHash('sha256').update(bytes).digest('hex') }
}

async function saveInventory(files: InventoryFile[]): Promise<Inventory> {
  const inventory = { version: 1, bytes: files.reduce((sum, file) => sum + file.bytes, 0), files }
  await writeFile(join(stage, 'toolpack-files.json'), JSON.stringify(inventory))
  return inventory
}

async function makeStage(options: { tools?: ManifestTool[]; payload?: Buffer } = {}): Promise<InventoryFile[]> {
  const manifest = JSON.stringify({ version: 1, tools: options.tools ?? [
    { id: 'demo', roots: ['tools/demo', 'runtime/windows/test-runtime'], preserve: ['tools/demo/settings/user.ini'] },
  ] })
  const files = [
    await writeStage('tools/demo/data/sample.bin', options.payload ?? randomBytes(24 * 1024)),
    await writeStage('runtime/windows/test-runtime/runtime.txt', 'fixture runtime'),
    await writeStage('tools/manifest.json', manifest),
    await writeStage('tools/verified.json', JSON.stringify({ version: 1,
      catalogSha256: createHash('sha256').update(manifest).digest('hex'), tools: ['demo'] })),
  ]
  await saveInventory(files)
  return files
}

async function build(overrides: { stage?: string; output?: string; previous?: string } = {}): Promise<ToolPackMetadataV2> {
  const directory = overrides.output ?? output
  await execute(process.execPath, [script, '--stage', overrides.stage ?? stage, '--output', directory,
    ...overrides.previous === undefined ? [] : ['--previous', overrides.previous]], { windowsHide: true })
  const metadata = toolPackMetadataSchema.parse(JSON.parse(await readFile(join(directory, 'native-tools-metadata.json'), 'utf8')))
  if (metadata.version !== 2) throw new Error('Expected per-unit metadata')
  return metadata
}

async function archiveBytes(metadata: ToolPackMetadataV2, directory = output): Promise<Buffer[]> {
  const buffers = []
  for (const archive of metadata.archives) {
    const bytes = await readFile(join(directory, archive.file))
    expect(bytes.length).toBe(archive.bytes)
    expect(createHash('sha256').update(bytes).digest('hex')).toBe(archive.sha256)
    expect((await lstat(join(directory, archive.file))).nlink).toBe(1)
    buffers.push(bytes)
  }
  return buffers
}

async function entries(gzip: Buffer): Promise<{ path: string; type: string; mtime: number | undefined; bytes: Buffer }[]> {
  const found: { path: string; type: string; mtime: number | undefined; bytes: Buffer }[] = []
  const parser = listTar({ strict: true, onReadEntry: (entry) => {
    const chunks: Buffer[] = []
    entry.on('data', (chunk: Buffer) => { chunks.push(chunk) })
    entry.on('end', () => { found.push({ path: entry.path, type: entry.type, mtime: entry.mtime?.getTime(), bytes: Buffer.concat(chunks) }) })
  } })
  await new Promise<void>((resolvePromise, reject) => {
    parser.on('error', reject)
    parser.on('end', resolvePromise)
    parser.end(gzip)
  })
  return found
}

describe('native tool archive CLI', () => {
  it('writes one archive per unit with only its sorted inventoried files and fixed timestamps', async () => {
    const files = await makeStage()
    await writeStage('tools/demo/private-cache.txt', 'not inventoried')
    const metadata = await build()
    expect(metadata.files.map(file => file.path)).toEqual(files.map(file => file.path).sort())
    expect(metadata.units).toEqual([
      { path: 'runtime/windows/test-runtime', kind: 'directory', preserve: [] },
      { path: 'tools/demo', kind: 'directory', preserve: ['tools/demo/settings/user.ini'] },
      { path: 'tools/manifest.json', kind: 'file', preserve: [] },
      { path: 'tools/verified.json', kind: 'file', preserve: [] },
    ])
    expect(metadata.archives.map(archive => archive.unit)).toEqual(metadata.units.map(unit => unit.path))
    const archives = await archiveBytes(metadata)
    for (const [index, archive] of metadata.archives.entries()) {
      const found = await entries(archives[index] ?? Buffer.alloc(0))
      expect(found.map(entry => entry.path)).toEqual(metadata.files.map(file => file.path)
        .filter(path => path === archive.unit || path.startsWith(archive.unit + '/')))
      for (const entry of found) {
        expect(entry.type).toBe('File')
        expect(entry.mtime).toBe(0)
        expect(entry.bytes).toEqual(await readFile(join(stage, ...entry.path.split('/'))))
      }
    }
    expect(await readdir(output)).toEqual(expect.arrayContaining(['native-tools-metadata.json', ...metadata.archives.map(archive => archive.file)]))
    expect((await readdir(output)).some(file => file.startsWith('.native-toolpack-'))).toBe(false)
  })

  it('reproduces bytes despite source mtimes and keeps the archive name of every unchanged unit', async () => {
    const files = await makeStage()
    const first = await build()
    const firstBytes = await archiveBytes(first)
    await utimes(join(stage, files[0].path), new Date(0), new Date(1_500_000))
    const second = await build()
    expect(second).toEqual(first)
    expect(await archiveBytes(second)).toEqual(firstBytes)
    const changed = await writeStage('tools/demo/data/sample.bin', Buffer.from('a new release'))
    await saveInventory(files.map(file => file.path === changed.path ? changed : file))
    const third = await build()
    expect(third.id).not.toBe(first.id)
    const renamed = third.archives.filter((archive, index) => archive.file !== first.archives[index]?.file).map(archive => archive.unit)
    expect(renamed).toEqual(['tools/demo'])
    expect(await archiveBytes(first)).toEqual(firstBytes)
    expect(await readFile(join(output, 'native-tools-metadata.json'), 'utf8')).toContain(third.id)
  })

  it('reuses the archive records of unchanged units from earlier metadata without writing them again', async () => {
    const files = await makeStage()
    const first = await build()
    const earlier = join(fixture, 'earlier-metadata.json')
    await writeFile(earlier, JSON.stringify(first))
    const changed = await writeStage('tools/demo/data/sample.bin', Buffer.from('a new release'))
    await saveInventory(files.map(file => file.path === changed.path ? changed : file))
    const next = join(fixture, 'next output')
    const second = await build({ output: next, previous: earlier })
    expect(second.archives.filter(archive => archive.unit !== 'tools/demo')).toEqual(first.archives.filter(archive => archive.unit !== 'tools/demo'))
    expect((await readdir(next)).sort()).toEqual(['native-tools-metadata.json', second.archives.find(archive => archive.unit === 'tools/demo')?.file].sort())
  })

  it('allows independent builders to publish the same deterministic pack concurrently', async () => {
    await makeStage()
    const [first, second] = await Promise.all([build(), build()])
    expect(first).toEqual(second)
    await archiveBytes(first)
    expect((await readdir(output)).filter(file => file.startsWith('.native-toolpack-'))).toEqual([])
  })

  it('does not replace existing metadata when a unit archive conflicts', async () => {
    await makeStage()
    const original = await build()
    const before = await readFile(join(output, 'native-tools-metadata.json'), 'utf8')
    await writeFile(join(output, original.archives[0]?.file ?? ''), 'damaged existing archive')
    await expect(build()).rejects.toThrow('Existing unit archive has different bytes')
    expect(await readFile(join(output, 'native-tools-metadata.json'), 'utf8')).toBe(before)
    expect((await readdir(output)).some(file => file.startsWith('.native-toolpack-'))).toBe(false)
  })

  it('refuses changed file sizes, hashes and mismatched inventory totals before publication', async () => {
    const files = await makeStage()
    const original = files[0]
    await saveInventory(files.map(file => file === original ? { ...file, bytes: file.bytes + 1 } : file))
    await expect(build()).rejects.toThrow('Source byte count does not match')
    await saveInventory(files.map(file => file === original ? { ...file, sha256: '0'.repeat(64) } : file))
    await expect(build()).rejects.toThrow('Source SHA-256 does not match')
    const inventory = await saveInventory(files)
    await writeFile(join(stage, 'toolpack-files.json'), JSON.stringify({ ...inventory, bytes: inventory.bytes + 1 }))
    await expect(build()).rejects.toThrow('inventory byte total does not match')
    await expect(lstat(output)).rejects.toMatchObject({ code: 'ENOENT' })
  })

  it.each(['../escape', '/absolute', 'C:/drive', 'tools/demo/file:stream', 'tools\\demo\\file',
    'tools/demo/../file', 'tools/demo/NUL.txt', 'tools/demo/COM¹.log', 'tools/demo/trailing.', 'tools/demo/trailing ',
    'tools/demo/bad?name', 'tools/demo//empty'])('refuses unsafe path %s', async (path) => {
    const files = await makeStage()
    await saveInventory([{ ...files[0], path }, ...files.slice(1)])
    await expect(build()).rejects.toThrow('Windows installation-relative path')
  })

  it('refuses file aliases and directory names with conflicting Windows casing', async () => {
    const files = await makeStage()
    await saveInventory([...files, { ...files[0], path: files[0].path.toUpperCase() }])
    await expect(build()).rejects.toThrow('Duplicate case-insensitive inventory path')
    await saveInventory([...files, { ...files[0], path: 'tools/demo/Data/second.bin' }])
    await expect(build()).rejects.toThrow('Case-insensitive path alias')
  })

  it.each([
    { id: 'demo', roots: ['tools/demo', 'runtime/linux/python'] },
    { id: 'demo', roots: ['tools/demo'], preserve: ['tools/other/user.ini'] },
    { id: 'demo', roots: ['tools/demo'], preserve: ['tools/demo/settings', 'tools/demo/settings/user.ini'] },
  ])('refuses invalid roots or preserved paths: %j', async (tool) => {
    await makeStage({ tools: [tool] })
    await expect(build()).rejects.toThrow(/Unsupported installation directory|Preserved path is outside|Overlapping preserved paths/)
  })

  it('refuses inventory files outside declared units', async () => {
    const files = await makeStage()
    files.push(await writeStage('tools/other/undeclared.txt', 'not a declared tool'))
    await saveInventory(files)
    await expect(build()).rejects.toThrow('outside the installation directories')
  })

  it('refuses junctions in the stage or any payload ancestor', async () => {
    await makeStage()
    const linkedStage = join(fixture, 'linked-stage')
    await symlink(stage, linkedStage, 'junction')
    await expect(build({ stage: linkedStage })).rejects.toThrow('symlink or junction')
    const payload = join(stage, 'tools/demo')
    const moved = join(fixture, 'independent-payload')
    for (const candidate of [payload, moved]) {
      if (relative(fixture, resolve(candidate)).startsWith('..')) throw new Error('Fixture move escaped the test directory.')
    }
    await rename(payload, moved)
    await symlink(moved, payload, 'junction')
    await expect(build()).rejects.toThrow('symlink or junction')
  })

  it('refuses hardlinked payload files and linked output ancestors', async () => {
    const files = await makeStage()
    await link(join(stage, files[0].path), join(fixture, 'hardlinked-payload'))
    await expect(build()).rejects.toThrow('independent regular file')
    await rm(join(fixture, 'hardlinked-payload'))
    const actualOutput = join(fixture, 'actual-output')
    await mkdir(actualOutput)
    await symlink(actualOutput, output, 'junction')
    await expect(build()).rejects.toThrow('symlink or junction')
  })

  it('validates input field types, the output location and earlier metadata', async () => {
    const files = await makeStage()
    const inventory = await saveInventory(files)
    await writeFile(join(stage, 'toolpack-files.json'), JSON.stringify({ ...inventory, bytes: String(inventory.bytes) }))
    await expect(build()).rejects.toThrow('version 1 SHA-256 file inventory')
    await saveInventory(files)
    await expect(build({ output: join(stage, 'output') })).rejects.toThrow('outside the source stage')
    await writeFile(join(fixture, 'v1.json'), JSON.stringify({ version: 1, volumes: [] }))
    await expect(build({ previous: join(fixture, 'v1.json') })).rejects.toThrow('version 2 tool pack')
  })

  it('can be imported without starting a build', async () => {
    const result = await execute(process.execPath, ['--input-type=module', '-e',
      `const value = await import(${JSON.stringify(pathToFileURL(script).href)}); console.log(typeof value.packageNativeTools)`], { cwd: fixture, windowsHide: true })
    expect(result.stdout.trim()).toBe('function')
    expect(await readdir(fixture)).toEqual(['离线工具 stage'])
  })
})
