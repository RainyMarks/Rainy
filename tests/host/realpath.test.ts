/** Folders on drives Windows' mount manager does not know (forensic image mounters such as 火眼, RAM disks) still resolve. */
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { realpathSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterAll, describe, expect, it, vi } from 'vitest'

// GetFinalPathNameByHandle fails with ERROR_INVALID_FUNCTION on such a volume, which libuv reports as EISDIR, or with
// ERROR_PATH_NOT_FOUND (ENOENT) when the volume has no DOS name. Paths containing "virtual-drive" behave like that here.
vi.mock('node:fs/promises', async (original) => {
  const actual = await original<typeof import('node:fs/promises')>()
  return {
    ...actual,
    realpath: vi.fn(async (path: string) => {
      if (String(path).includes('virtual-drive-eisdir')) throw Object.assign(new Error(`EISDIR: illegal operation on a directory, realpath '${path}'`), { code: 'EISDIR' })
      if (String(path).includes('virtual-drive-enoent')) throw Object.assign(new Error(`ENOENT: no such file or directory, realpath '${path}'`), { code: 'ENOENT' })
      return actual.realpath(path)
    }),
  }
})

const { realpath } = await import('../../src/host/realpath.ts')
const { Projects } = await import('../../src/host/projects.ts')

const base = realpathSync(await mkdtemp(join(tmpdir(), 'rainy-realpath-')))
afterAll(async () => { await rm(base, { recursive: true, force: true }) })

describe('realpath on drives the mount manager does not know', () => {
  it('answers with the walked path when the native call fails but the folder is there', async () => {
    for (const name of ['virtual-drive-eisdir', 'virtual-drive-enoent']) {
      const folder = join(base, name, 'case 1')
      await mkdir(folder, { recursive: true })
      expect(await realpath(folder, 'win32')).toBe(folder)
    }
  })

  it('keeps the native error for a path that is not there, and on Linux', async () => {
    await expect(realpath(join(base, 'virtual-drive-enoent', 'missing'), 'win32')).rejects.toMatchObject({ code: 'ENOENT' })
    await mkdir(join(base, 'virtual-drive-eisdir', 'linux'), { recursive: true })
    await expect(realpath(join(base, 'virtual-drive-eisdir', 'linux'), 'linux')).rejects.toMatchObject({ code: 'EISDIR' })
  })

  it.runIf(process.platform === 'win32')('opens such a folder as a project instead of saying it is not a folder', async () => {
    const folder = join(base, 'virtual-drive-eisdir', 'evidence')
    await mkdir(folder, { recursive: true })
    const projects = new Projects(join(base, 'projects.json'))
    await projects.load()
    const project = await projects.open(folder)
    expect(project.path).toBe(folder)
  })
})
